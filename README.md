# CedroDB

Motor de base de datos local escrito en Rust: documentos JSON, transacciones persistentes, consultas indexadas y un flujo de cambios que permite reanudar automatizaciones después de reiniciar. Incluye librerías nativas para Node.js/TypeScript y Python.

**Versión 0.2.0 experimental.** Es un motor nuevo con registro de almacenamiento propio. Todavía no está validado para producción y no se ha demostrado superioridad frente a SQLite, libmdbx, UnQLite o DuckDB.

## Instalar en Node.js o Python

```sh
npm install https://www.ltavarez.me/downloads/cedrodb-0.2.0.tgz
python -m pip install https://www.ltavarez.me/downloads/cedrodb-0.2.0.tar.gz
```

Estos comandos usan los paquetes descargables de la release. La publicación en los registros npm/PyPI requiere configurar las cuentas; no se anuncia como realizada. Node usa un binario incluido si coincide con su plataforma y compila desde fuente cuando no lo hay. El sdist de Python requiere compilar; los wheels compatibles evitan ese paso. Consulta [los requisitos, ejemplos y publicación de las librerías](docs/native-libraries.md).

El diferenciador del proyecto es combinar almacenamiento de documentos y automatizaciones locales sobre el mismo registro: guardar un cambio y su evento en un lote atómico, consultar el historial por cursor y continuar desde el último checkpoint. Es una dirección de producto; no una afirmación de exclusividad frente a todas las bases de datos.

## Ejecutar

Requiere Rust y Cargo. Desde esta carpeta:

```sh
cargo test --locked
cargo run --locked --example basic -- ./cedro-data
cargo run --locked --example portfolio_demo
```

El ejemplo `basic` crea o abre el directorio indicado, guarda dos pedidos y consulta sus importes. `portfolio_demo` usa un directorio temporal propio, confirma tres pedidos y un evento outbox en el mismo lote, descarta una transacción y vuelve a abrir la base. Después ejecuta una consulta indexada, un compare-and-exchange y lee el changefeed antes de emitir JSON. Ese resultado alimenta el ejemplo del portafolio; el navegador muestra una ejecución guardada, no ejecuta el motor Rust.

## Usar como dependencia local

El paquete todavía no se ha publicado en crates.io. Después de descargar o copiar el código:

```toml
[dependencies]
cedrodb = { path = "../cedrodb" }
serde_json = "1"
```

```rust
use cedrodb::Database;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Database::open("orders.cedro")?; // Es un directorio.
    let mut tx = db.begin_write()?;
    tx.put("orders", "order:001", json!({
        "customer": "Ana", "amount": 125.5, "status": "paid"
    }))?;
    tx.put("orders", "order:002", json!({
        "customer": "Luis", "amount": 74.5, "status": "paid"
    }))?;
    tx.commit()?;

    assert_eq!(db.aggregate("orders", "amount")?.sum, 200.0);
    println!("{:?}", db.get("orders", "order:001")?);
    Ok(())
}
```

## Capacidades actuales

- Colecciones con claves UTF-8 y valores JSON, incluidos objetos, listas y escalares.
- `put`, reemplazo y `delete` agrupados en una transacción. Descartarla sin `commit` abandona sus operaciones.
- Lecturas del estado confirmado y recorridos ordenados por clave.
- Registro append-only con versión, longitud y CRC32 por transacción.
- Confirmación con `sync_all` antes de publicar los cambios en memoria.
- Recuperación de una cola incompleta al abrir; un frame completo con checksum inválido produce error.
- Lock de archivo exclusivo para impedir otra apertura cooperativa del mismo almacenamiento.
- Agregaciones `count`, `sum`, `min`, `max` de un campo numérico de primer nivel.
- Consultas con filtros `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `exists`, combinados con AND; campos directos o JSON Pointer (`/profile/city`).
- Paginación ordenada por clave mediante un cursor exclusivo `after`, con hasta 1,000 resultados por página.
- Índices de igualdad por sesión, actualizados junto con el estado confirmado y seleccionados por cantidad de candidatos.
- Compare-and-exchange atómico para actualizar o eliminar sólo si el documento esperado sigue vigente.
- Changefeed durable con cursores por transacción y páginas limitadas por cantidad y bytes codificados.
- Estadísticas de documentos, colecciones, bytes de contenido, registro, cursores e índices.

La API es síncrona. El handle puede compartirse entre hilos; un mutex serializa las operaciones. Sólo puede existir una transacción de escritura activa y una apertura del almacenamiento. Una llamada de lectura observa un estado confirmado; varias llamadas o páginas no forman un snapshot común. Los índices se crean con `create_index` después de abrir; no se guardan sus definiciones en disco. Hay un máximo de 64 índices por handle y 64 filtros por consulta.

## Cambios durables y actualizaciones condicionales

```rust
use cedrodb::Database;
use serde_json::json;

fn main() -> cedrodb::Result<()> {
    let db = Database::open("cedro-data")?;
    db.put("jobs", "job:001", json!({"status":"pending"}))?;
    let claimed = db.compare_exchange(
        "jobs", "job:001",
        Some(json!({"status":"pending"})),
        Some(json!({"status":"running"})),
    )?;
    let page = db.changes(0, 100)?;
    println!("claimed={claimed}, checkpoint={}", page.next_cursor);
    Ok(())
}
```

El cursor comienza en 0 y es exclusivo; identifica el orden de frames completos, sobrevive a la reapertura y pertenece al historial concreto de esa base. `next_cursor` avanza sólo por transacciones devueltas. Un lote vacío y un CAS que no coincide no añaden frames. Cada página tiene como máximo `max_frame_bytes + 12` bytes de frames codificados y nunca parte una transacción. La estructura JSON decodificada y las copias del SDK consumen memoria adicional.

La aplicación guarda su checkpoint después de procesar los cambios. Entregar a un servicio externo y guardar ese checkpoint no constituye una sola transacción: los consumidores deben manejar reintentos y deduplicación. El outbox garantiza que el registro del evento y el cambio local se confirman juntos; no garantiza efectos externos exactamente una vez.

## Recursos y números

`Database::open_with_options(path, Options { .. })` permite limitar el contenido codificado:

| Límite predeterminado | Tamaño |
| --- | --- |
| Documento JSON | 1 MiB |
| Payload de transacción | 16 MiB |
| Contenido del estado confirmado | 64 MiB |
| Nombre de colección | 1–1,024 bytes UTF-8 |
| Clave | 1–4,096 bytes UTF-8 |

El límite de estado cuenta valores JSON codificados y nombres de colección/clave por registro. **No es un límite de RAM del proceso:** mapas, strings, buffers, operaciones pendientes y copias de resultados consumen memoria adicional. Todos los documentos confirmados permanecen en memoria. Los índices también usan RAM; el changefeed conserva un offset de ocho bytes por transacción confirmada, sin duplicar los documentos históricos. `scan` devuelve una copia de la colección. El registro crece con el historial y aún no hay compactación.

La profundidad máxima de documentos es 64. Al reabrir, los límites deben permitir reproducir los documentos y las transacciones ya guardados; un límite nuevo más bajo puede impedir la apertura.

Las agregaciones convierten números a `f64`, ignoran campos ausentes, nulos y no numéricos, y rechazan sumas no finitas. `count` cuenta valores numéricos, no todos los documentos. Los enteros mayores que 2^53 pueden perder precisión durante la agregación; su valor JSON almacenado se conserva. Para importes que requieran aritmética exacta hace falta un agregado entero/decimal específico, previsto para otra versión.

Eq/Ne/CAS comparan JSON recursivamente; enteros y flotantes equivalentes dentro de ±2^53 coinciden, para que `1.0` escrito por Python pueda compararse con `1` de JavaScript. Booleanos y números son distintos. Los enteros grandes se comparan exactamente entre sí; su orden frente a flotantes se rechaza. Los filtros Eq/Ne/Exists se resuelven antes de los ordenados, por lo que crear un índice no introduce errores de conversión en documentos ya excluidos.

## Persistencia y recuperación

Cada transacción confirmada ocupa un frame completo del registro `cedro.log`. Una reapertura aplica exclusivamente frames completos y válidos. Si el final contiene una escritura incompleta, se trunca hasta la última transacción completa. Un checksum inválido, versión desconocida, JSON inválido o longitud fuera del límite no se repara silenciosamente.

Ante un error de escritura o sincronización, el handle queda inutilizable hasta cerrarlo y reabrirlo. El resultado de esa transacción puede ser incierto: la reapertura determina qué frame quedó completo. La aplicación debe contemplarlo al decidir un reintento.

Las garantías dependen del sistema operativo, del sistema de archivos y del dispositivo de almacenamiento. Las pruebas de terminación de proceso y truncamiento de archivos no certifican comportamiento ante cortes eléctricos reales. Usa un sistema de archivos local; no edites, sustituyas ni borres el registro mientras está abierto. CRC32 detecta corrupción accidental y no autentica datos.

## Diseño y evolución

Tomamos como referencias el almacenamiento transaccional de libmdbx, la integración de documentos de UnQLite y el procesamiento analítico de DuckDB. Esta primera versión implementa un subconjunto propio; no combina sus motores ni hereda sus garantías.

1. **0.1:** registro transaccional, documentos, recuperación y agregaciones por escaneo.
2. **0.2 actual:** consultas paginadas, índices de igualdad por sesión, CAS, changefeed y SDKs nativos Node/Python.
3. **Siguiente etapa:** más inyección de fallos de I/O, fuzzing, mediciones e índices persistentes.
4. **Después de medir:** compactación con una política explícita de cursores/retención, consultas vectorizadas e interfaz C.

SQL, MVCC, replicación, cifrado, consultas distribuidas y sincronización entre dispositivos no están implementados. El log 0.2 conserva el formato 0.1; futuras versiones anteriores a la estabilidad pueden cambiarlo. No hay migrador ni política de compactación/retención todavía.

Consulta [la arquitectura](docs/architecture.md) y [el protocolo de comparación](docs/benchmarks.md).

## Regenerar los archivos del portafolio

```sh
python3 scripts/package-portfolio.py --portfolio ../portfolio-loading --sdk-dir dist
```

El script ejecuta el ejemplo Rust, valida el resultado y crea `public/demos/cedrodb-results.json`, el ZIP de código fuente descargable y su SHA-256. Requiere Python 3.9 o superior. No incluye bases de datos, dependencias compiladas ni archivos del entorno local.

## Licencia

MIT. Las dependencias conservan sus respectivas licencias.
