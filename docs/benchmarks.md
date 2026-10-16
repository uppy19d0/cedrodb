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

## Comparación entre motores pendiente

No hay comparaciones publicadas que demuestren superioridad frente a otros
motores. Estas son las condiciones necesarias para una comparación útil.

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
