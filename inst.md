# Instrucciones de uso

Este proyecto usa `he1-unificar-pdfs` para procesar PDFs por carpeta.

Hay dos modos de entrada:

1. Una carpeta.
2. Un archivo `.txt` con una ruta por linea.

## Modo carpeta

Si la entrada es una carpeta, el programa procesa esa carpeta como una unidad.

- Solo revisa los PDFs que estan directamente dentro de esa carpeta.
- No mueve los archivos fuera de su ubicacion original.
- Los PDFs que no cumplen la regla canonica se dejan intactos.

## Modo lista de carpetas

Si la entrada es un archivo `.txt`, cada linea valida se interpreta como una carpeta.

- Las lineas vacias se ignoran.
- Las lineas que empiezan con `#` se ignoran.
- Las rutas relativas se resuelven respecto a la carpeta donde esta el `.txt`.
- Cada carpeta listada se procesa de forma independiente.

Ejemplo de archivo:

```txt
G:\codex_projects\rust_cambia_nombre_planillas_he1\pdfs\folder_0001
G:\codex_projects\rust_cambia_nombre_planillas_he1\pdfs\folder_0002
G:\codex_projects\rust_cambia_nombre_planillas_he1\pdfs\folder_0003
```

## Respaldo

Antes de tocar un archivo original, el programa guarda una copia de respaldo.

- El respaldo se guarda en una carpeta oculta `.he1_respaldo`.
- Cada corrida crea una carpeta propia con nombre tipo `run_<timestamp>_<pid>`.
- El respaldo incluye un `manifest.txt` con la relacion entre originales y copias.
- Solo se respaldan los PDFs que realmente se van a tocar.

## Restauracion

Para volver al estado previo, se puede ejecutar:

```bash
he1-unificar-pdfs --restore <ruta_respaldo_o_manifest.txt>
```

El comando de restauracion:

- recupera los PDFs originales desde el respaldo;
- elimina los PDFs generados que no existian antes;
- limpia marcas temporales como `.he1_procesado` y `.he1_aux_temporal`.

## Archivo de evidencia

El programa deja un `Cambios.txt` en la carpeta contenedora del origen.

- Si la entrada es una carpeta, `Cambios.txt` queda en la carpeta padre.
- Si la entrada es un `.txt`, `Cambios.txt` queda junto al archivo de lista.

## Regla de nombres

Los nombres canonicos validos estan documentados en `REGLA_UNIFICACION.md`.

La regla operativa actual es:

- `base.pdf` se acepta;
- `base_*.pdf` o `base-*.pdf` se acepta;
- `AES (copia).pdf` y `PI.copia.pdf` tambien pueden reducirse al canonico por normalizacion;
- `PI13.pdf` y `AES4545.pdf` no se aceptan porque no cumplen la regla con `_` ni con `-`.
