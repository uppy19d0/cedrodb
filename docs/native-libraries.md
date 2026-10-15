# Librerías nativas: Node.js y Python

Las dos librerías llaman directamente a CedroDB Rust. No ejecutan un servidor, no mantienen otro motor y no crean una copia de la base en JavaScript o Python. La API es síncrona y el formato de disco es el mismo.

## Distribuciones 0.5.0

La librería Node.js está publicada en [npm como `cedrodb@0.5.0`](https://www.npmjs.com/package/cedrodb/v/0.5.0). Python se instala desde los paquetes descargables de la release; su publicación en PyPI está pendiente:

```sh
npm install cedrodb@0.5.0
python -m pip install https://www.ltavarez.me/downloads/cedrodb-0.5.0.tar.gz
```

El paquete npm contiene fuentes Rust y carga un binario compatible incluido cuando está disponible. Si no lo hay, compila al instalar: se necesitan Rust 1.88+, Cargo y un enlazador C de la plataforma. El sdist de Python necesita Rust 1.85+; un wheel compatible permite instalar sin compilar. Consulta los binarios y checksums de la release antes de elegir plataforma. Los binarios GNU/Linux generados en Ubuntu 22.04 requieren un entorno de glibc compatible; no se anuncian como binarios musl.

El manifiesto del portafolio `demos/cedrodb-artifacts.json` enumera los archivos disponibles y el estado de publicación en cada registro.

## Node.js / TypeScript

```js
import { Database } from 'cedrodb';

const db = new Database('./cedro-data');
try {
  const orders = db.collection('orders');
  db.batch([
    { kind: 'put', collection: 'orders', key: '001',
      value: { amount: 125.5, status: 'paid' } },
    { kind: 'put', collection: 'outbox', key: 'event:001',
      value: { type: 'order.created', orderId: '001' } }
  ]);
  orders.createIndex('status');
  const page = orders.findPage({
    filters: [{ field: 'status', op: 'eq', value: 'paid' }],
    limit: 20
  });
  console.log(page.records, page.usedIndex, page.nextAfter);
  const changes = db.changes({ since: 0, limit: 50 });
  console.log(changes.transactions, changes.nextCursor);
} finally {
  db.close();
}
```

`get` devuelve `undefined` para una clave ausente y `null` para un JSON null guardado. `compareExchange(key, expected, replacement)` usa `undefined` como ausencia/eliminación. El paquete rechaza números enteros fuera del rango seguro de JavaScript, incluyendo valores leídos desde archivos escritos por Python/Rust; así evita devolver enteros redondeados silenciosamente. Los tipos ESM y CommonJS están incluidos.

## Python

```python
from cedrodb import Database, MISSING

with Database('./cedro-data') as db:
    orders = db.collection('orders')
    orders.put_many({'001': {'amount': 125.5, 'status': 'paid'}})
    orders.create_index('status')
    page = orders.find_page(
        [{'field': 'status', 'op': 'eq', 'value': 'paid'}],
        limit=20,
    )
    print(page['records'], page['used_index'])
    inserted = orders.compare_exchange(
        '002', expected=MISSING,
        replacement={'amount': 74.5, 'status': 'pending'},
    )
    print(inserted, db.changes(since=0)['next_cursor'])
```

`get(key, MISSING)` distingue ausencia de JSON null (`None`). `expected=MISSING` significa que el registro no debe existir; `replacement=DELETE` lo elimina. Python conserva enteros en [-2^63, 2^64−1]; booleanos no se confunden con enteros. `with db.transaction() as tx` ofrece lotes de escritura con rollback ante excepciones; no añade aislamiento de lecturas ni vistas de cambios pendientes.

## Uso y restricciones comunes

- Un único handle/proceso abre la base a la vez. Cierra Node antes de abrir el mismo directorio desde Python, y viceversa.
- Los índices se crean por sesión y se reconstruyen con `createIndex` / `create_index` después de abrir.
- Los filtros se combinan con AND; aceptan campos directos y JSON Pointer (`/profile/city`). La paginación usa una clave exclusiva y no fija un snapshot entre llamadas.
- Las igualdades de números enteros/flotantes equivalentes dentro de ±2^53 son compatibles entre lenguajes. El orden entre flotantes y enteros fuera de ese rango se rechaza para evitar redondeos.
- `changes` devuelve transacciones completas después del cursor, con límite de cantidad y de bytes codificados por página. El checkpoint y la entrega a sistemas externos pertenecen a la aplicación.
- Las operaciones síncronas de disco pueden bloquear el hilo llamante. En servidores con latencia sensible, ejecuta el trabajo en un worker dedicado y conserva allí el propietario de la base.

Consulta las referencias completas en [Node](../bindings/node/README.md) y [Python](../bindings/python/README.md).

## Publicar en los registros

El workflow manual `publish-registries.yml` toma los artefactos de una ejecución CI validada; no vuelve a construir otra versión para publicarla.

Para futuras versiones npm, publica el `.tgz` probado desde una sesión autenticada con 2FA, o configura un trusted publisher con permiso para `npm publish` en el job `npm`. Ese job también admite `NPM_TOKEN` en el entorno GitHub `npm` con los permisos de publicación correspondientes.

Para la primera publicación en PyPI, registra un publisher pendiente para el proyecto `cedrodb` con owner `uppy19d0`, repositorio `cedrodb`, workflow `publish-registries.yml` y entorno `pypi`. Ese registro requiere acceso a la cuenta PyPI. El workflow usa OIDC y no necesita un token guardado en el código.

Activar un job de publicación sin configurar su cuenta fallará; no se presenta como una publicación realizada.
