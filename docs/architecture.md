# Arquitectura de CedroDB 0.2

## Modelo

Una base es un directorio con un archivo `cedro.log`. Su apertura toma un lock exclusivo del sistema operativo, carga los frames confirmados y reconstruye un `BTreeMap` por colección. Los valores son `serde_json::Value`; el mapa ordena las claves, sin índices por campos de documentos.

```text
API Rust
  ├─ get / scan / aggregate
  └─ begin_write → put / delete → commit
                     │
              Límites y validación
                     │
       Frame transaccional + sync_all
                     │
            Estado confirmado en RAM
```

Un mutex protege el archivo, el estado y la exclusión del escritor. El transaction builder acumula operaciones y permite lecturas del estado anterior a su confirmación. El commit aplica el lote completo y actualiza los índices de igualdad bajo el mutex. CAS compara y confirma bajo ese mismo mutex. No hay snapshots de lectura ni MVCC.

Las librerías N-API y PyO3 son adaptadores nativos de esta API. Sus paquetes incluyen fuentes del mismo núcleo y usan el mismo formato. La interfaz Node valida los números antes de convertirlos a JavaScript; Python conserva los enteros JSON i64/u64.

## Consultas y changefeed

Una consulta evalúa filtros AND y devuelve una página ordenada por clave. Si hay índices de igualdad aplicables, elige el conjunto de candidatos más pequeño. Los índices son efímeros, se actualizan con los commits y se crean de nuevo al abrir. El campo `scanned` cuenta candidatos inspeccionados, incluido un posible lookahead de paginación. Un índice reduce candidatos en igualdad; no es un índice de rangos ni un almacén columnar.

El changefeed reutiliza los frames del registro. Un vector de offsets u64 permite localizar cada transacción sin duplicar sus documentos históricos. Los cursores son ordinales, comienzan en uno y permanecen estables mientras se conserva ese historial. Leer cambios mueve el cursor del descriptor; cada commit busca explícitamente EOF antes de añadir datos.

Una página respeta el límite solicitado de transacciones y un presupuesto de frames codificados de `max_frame_bytes + 12`. Las transacciones son indivisibles. Los futuros cambios de retención/compactación deberán definir cómo se conservan o invalidan cursores; no están implementados.

## Formato experimental

Todos los enteros del framing usan little endian.

| Campo | Bytes | Contenido |
| --- | --- | --- |
| Magic | 8 | `CEDRODB\0` |
| Versión | 4 | `1` |
| Reservado | 4 | `0` |
| Longitud de frame | 4 | Bytes del payload JSON |
| Complemento de longitud | 4 | NOT bit a bit de la longitud |
| CRC32 | 4 | Checksum del payload |
| Payload | variable | `{"ops":[...]}` |

El header de archivo aparece una vez; los frames se repiten hasta el final. Una operación contiene `kind`, `collection`, `key` y, para `put`, `value`. El lote conserva el orden de las operaciones. Los campos de framing y CRC32 no constituyen un protocolo criptográfico ni una garantía frente a alteraciones deliberadas.

## Confirmación

1. Validar las operaciones y sus límites antes de escribir.
2. Preparar el cambio sobre las claves afectadas y verificar el tamaño de contenido resultante.
3. Serializar el lote y calcular su checksum.
4. Añadir el frame completo y ejecutar `sync_all`.
5. Publicar el cambio en el mapa confirmado.

Un error de I/O durante la escritura o el sync inutiliza el handle. Se necesita reabrirlo para conocer el estado recuperable. No se intenta seguir añadiendo transacciones detrás de una escritura fallida. Un error por límite se devuelve antes de añadir un frame.

La sincronización de archivo y directorios durante la creación se realiza en Unix. La persistencia física depende de que el entorno respete las operaciones de sincronización. Crear varios directorios antecesores de golpe no equivale a certificar toda la cadena ante un corte eléctrico.

## Apertura y recuperación

El lector verifica el header, cada longitud y su complemento antes de reservar el payload. El complemento permite detectar un cambio accidental de la longitud antes de confundirlo con una cola incompleta. Aplica un lote sólo después de verificar el checksum y deserializarlo completamente. La cola truncada se recorta al último boundary válido. Los frames completos corruptos producen error y se conservan para inspección.

La detección tiene límites: un archivo truncado por causas externas puede ser indistinguible de una escritura interrumpida, y checksums/complementos pueden tener colisiones. No hay autenticación del archivo ni promesa de detección de toda alteración posible.

## Costos y restricciones

- `get`: búsqueda por clave en árbol, más copia del resultado.
- `scan`: recorre y copia los documentos de la colección.
- `aggregate`: recorre los valores de la colección bajo el mutex.
- `query`: recorre candidatos del índice o del mapa y copia una página de resultados.
- `changes`: lee y decodifica los frames solicitados, con presupuesto de bytes por página.
- Reapertura: reproduce el historial completo.
- Disco: conserva versiones y eliminaciones hasta que exista compactación.
- Memoria: documentos completos, índices de sesión y ocho bytes por transacción para offsets, más metadatos y buffers; los límites de contenido no limitan el RSS.

Estas decisiones simplifican el prototipo. Se necesitan resultados medidos antes de elegir otro almacenamiento, paralelizar consultas o mantener una segunda representación columnar.

## Validación pendiente

Además de las pruebas de API, corrupción y recuperación incluidas: inyección de fallos de escritura y sync, fuzzing, ensayos prolongados, apagones controlados en hardware y pruebas de sistemas de archivos. El prototipo no se presenta como almacenamiento de producción.
