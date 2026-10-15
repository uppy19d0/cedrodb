# Comparación reproducible prevista

No hay cifras de rendimiento publicadas todavía. Esta guía define las condiciones necesarias para una comparación útil.

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
