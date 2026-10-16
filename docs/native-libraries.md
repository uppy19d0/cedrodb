# Librerías nativas: Node.js y Python

Las dos librerías llaman directamente a CedroDB Rust. No ejecutan un servidor, no mantienen otro motor y no crean una copia de la base en JavaScript o Python. La API es síncrona y el formato de disco es el mismo.

## Distribuciones 0.6.0

Los comandos de instalación, una vez verificada la publicación, son [npm como `cedrodb@0.6.0`](https://www.npmjs.com/package/cedrodb/v/0.6.0) y en [PyPI como `cedrodb==0.6.0`](https://pypi.org/project/cedrodb/0.6.0/):

```sh
npm install cedrodb@0.6.0
python -m pip install cedrodb==0.6.0
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

La publicación npm se realiza manualmente desde una sesión autenticada con 2FA, usando el `.tgz` probado por CI. Se publica ese mismo archivo; no se reconstruye el paquete durante la publicación.

El trusted publisher de PyPI ya está configurado para el proyecto `cedrodb`, owner `uppy19d0`, repositorio `cedrodb`, workflow `publish-registries.yml` y entorno `pypi`. El job `pypi` usa OIDC para publicar los wheels y el sdist de la ejecución CI seleccionada, sin un token PyPI guardado en el código.

Después de publicar, comprueba que la versión y sus archivos estén disponibles en el registro antes de anunciar la release.
