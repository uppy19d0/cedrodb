# Herramientas de rendimiento y comparaciones

## Medir CedroDB 0.7 en tu equipo

La distribución Python incluye un diagnóstico del runtime y un benchmark que
crea una base temporal independiente de tus datos:

```sh
python -m cedrodb doctor
python -m cedrodb benchmark --records 1000 --iterations 30
python -m cedrodb benchmark --records 10000 --iterations 100 > benchmark.json
```

El benchmark compara consultas y búsqueda híbrida con y sin un índice de
igualdad. Verifica los documentos, puntuaciones y orden de resultados antes de
informar las medidas; también verifica integridad y cantidad tras reabrir.
Registra tamaño, lotes, calentamiento, versiones, sistema, CPU/arquitectura,
durabilidad, tiempo nativo y tiempo de llamada completa de Python. Incluye
latencias de lotes, construcción de índice y reapertura. Los vectores son
sintéticos y no miden calidad de respuestas de IA.

Los percentiles son resúmenes empíricos por rango más próximo; con pocas
muestras P99 puede ser simplemente el máximo observado. La herramienta no
controla cachés del sistema, no mide RSS/CPU y no realiza una comparación con
otros motores. `content_bytes` mide contenido codificado, no memoria física.
Registra hardware/disco/sistema de archivos por separado si vas a compartir un
informe. El tiempo usa el [contador de rendimiento de Python](https://docs.python.org/3/library/time.html#time.perf_counter_ns)
en el límite del SDK y `Instant` dentro del núcleo.

Para investigar una aplicación abierta, consulta los [diagnósticos y perfiles](performance.md).
Un índice puede reducir candidatos sin reducir la latencia en la misma
proporción: devolver documentos, puntuar, ordenar y convertir JSON sigue
costando trabajo. No conviertas una reducción de filas visitadas en una cifra
inventada de aceleración.

## Comparación con SQLite

El arnés reproducible está en [`benchmarks/sqlite-comparison`](../benchmarks/sqlite-comparison/README.md).
Es un crate independiente (el motor no gana dependencias) que ejecuta cada motor
en su propio proceso, verifica los resultados contra un modelo en memoria antes de
medir y compara solo configuraciones con la misma garantía de durabilidad:
`sync_all` en cada commit de CedroDB frente a SQLite con `synchronous=FULL` y
`fullfsync=ON`. La fila `synchronous=NORMAL` se muestra solo como referencia y no
es equivalente.

Primer resultado publicado: CedroDB 0.8.0 y SQLite 3.46.0 (compilado desde el
código empaquetado), Apple M2, macOS, compilación `release`, 100,000 documentos
JSON sintéticos, mediana [mínimo–máximo] de 3 ejecuciones. Los datos completos y
el entorno están en [`results/`](../benchmarks/sqlite-comparison/results).

| Métrica | CedroDB | SQLite WAL + FULL | SQLite DELETE + FULL |
| --- | ---: | ---: | ---: |
| Commit de 1 documento, p50 | 4.06 ms | 4.09 ms | 13.96 ms |
| Inserción, 100 documentos por commit | 19,808 doc/s | 18,760 doc/s | 6,469 doc/s |
| Inserción, 1,000 documentos por commit | 138,701 [131,232–139,557] doc/s | 142,341 [141,800–154,836] doc/s | 53,452 doc/s |
| Lectura por clave existente, p50 | 0.8 µs | 3.8 µs | 7.0 µs |
| Agregado count/sum/min/max, p50 | 6.9 ms | 95.5 ms | 96.7 ms |
| Reapertura con el historial | 201.9 ms | 0.4 ms | 0.2 ms |
| RSS máximo | 257 MiB [239–276] | 100.7 MiB | 100.4 MiB |
| Disco tras 10% de actualizaciones y 10% de borrados | 25.8 MiB | 29.0 MiB | 24.3 MiB |

Con 10,000 documentos el resultado es el mismo en forma: commits y lotes
empatados, lecturas y agregados más rápidos en CedroDB, reapertura de 21.0 ms
frente a 0.4 ms y una memoria máxima de 34.8 MiB frente a 16.5 MiB.

### Qué muestra y qué no

- Un commit durable cuesta lo mismo en ambos motores porque lo domina la espera
  del disco, no el motor. No hay ventaja de escritura.
- Las lecturas y los agregados de CedroDB son más rápidos porque todos los
  documentos activos están en RAM. Es una decisión de diseño con un precio: entre
  dos y dos veces y media la memoria en esta carga (la memoria máxima de CedroDB
  varía entre ejecuciones; no la interpretes con más precisión de la que tiene), el límite predeterminado de 64 MiB de estado
  vivo y la imposibilidad de superar la memoria disponible. SQLite lee desde su
  caché de páginas y no tiene esa restricción.
- La reapertura de CedroDB crece de forma lineal con el log, porque reproduce
  todas las transacciones. Es hoy su desventaja más clara.
- El log de CedroDB conserva el historial y no se compacta; el tamaño en disco
  solo es comparable mientras haya pocas actualizaciones.
- SQLite guarda aquí el JSON como texto y agrega con `json_extract`. Un esquema
  con columnas generadas o índices sobre el campo numérico reduciría la brecha del
  agregado, así que esa cifra no debe leerse como una ventaja general.
- Es una sola máquina, un solo disco, documentos sintéticos y caché del sistema
  caliente. No se midió el arranque en frío ni otro sistema operativo.

Nada de lo anterior demuestra una superioridad general frente a SQLite. Sigue
pendiente comparar con libmdbx, UnQLite y DuckDB, repetir en Linux y en otro
hardware, medir el arranque en frío y probar un esquema SQLite optimizado.

### Efecto de las optimizaciones de reapertura

Las ejecuciones de arriba usan el código final. Para atribuir la mejora de la
reapertura se comparó el código original de `main` con el final **sobre el mismo
log** (90,000 documentos vivos tras actualizaciones y borrados), alternando 10
ejecuciones de cada versión en procesos nuevos (`--reopen`): mediana de
277.4 ms [265.7–317.0] a 187.4 ms [180.6–199.2], es decir, un 32 % menos, con la
misma memoria al abrir (102 y 101 MiB). Se obtuvo midiendo cada documento una
sola vez, moviendo los documentos en lugar de clonarlos al reproducir el log y
leyendo las operaciones sin copiarlas a un búfer intermedio. El formato del log no
cambió. La reapertura sigue creciendo de forma lineal con el log.

### Reproducir

```sh
cd benchmarks/sqlite-comparison
./run.sh mi-equipo        # 3 ejecuciones por tamaño; DIR=/ruta para elegir el disco
python3 summarize.py results/*-mi-equipo-100000-run*.json
```

## Condiciones para comparar con otros motores

Estas son las condiciones necesarias para una comparación útil.

## Motores de referencia

- SQLite: filas y consultas equivalentes, índice por clave primaria.
- libmdbx: valores JSON y claves con el mismo contenido lógico.
- UnQLite: API clave–valor/documentos según la operación comparada.
- DuckDB: documentos o filas equivalentes para las agregaciones analíticas.

No comparar una escritura sincronizada de CedroDB con otra configuración que no sincronice a disco. Declarar siempre transacciones, índices, representación de datos y modo de durabilidad.

## Cargas

| Caso | Trabajo |
| --- | --- |
| Inserción | Lotes de 1, 100 y 1,000 documentos |
| Lectura por clave | Claves aleatorias existentes y ausentes |
| Actualización | Modificar 10% de claves existentes |
| Eliminación | Borrar 10% y volver a abrir |
| Análisis | Count/sum/min/max de un campo numérico |
| Recuperación | Abrir un historial con actualizaciones y eliminaciones |

Usar conjuntos deterministas de 1,000, 10,000 y 100,000 documentos, con el mismo generador y semilla. Si CedroDB supera un límite configurado, registrar el resultado como no admitido bajo esa configuración. No ocultar los fallos ajustando sólo un motor.

## Información que acompañará cada resultado

Versiones exactas de motores y compilador; flags de release; CPU, RAM, disco, sistema operativo y sistema de archivos; tamaño del conjunto y de los lotes; límites; método para calentamiento de caché; número de repeticiones; mediana y percentiles de latencia; throughput; RSS máximo; espacio en disco y costo de reapertura.

Separar ejecución con caché caliente y arranque frío. Verificar igualdad de resultados antes de comparar tiempos. La prueba de integridad y recuperación se reporta por separado del benchmark de velocidad.

## Criterio de decisión

El primer objetivo es un comportamiento correcto y una API útil. Un cambio de arquitectura debe resolver un costo medido. La ventaja de CedroDB sólo se anunciará cuando las cargas reproducibles la demuestren y sus restricciones estén publicadas.
