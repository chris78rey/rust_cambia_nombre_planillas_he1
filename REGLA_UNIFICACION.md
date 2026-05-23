# Regla de unificacion y uso

Este es el unico documento de ayuda del proyecto. Resume las reglas que aplica `src/main.rs`.

## Comandos

- `he1-unificar-pdfs --label <etiqueta> <ruta.txt | carpeta>`
- `he1-unificar-pdfs --restore <etiqueta | ruta_respaldo_o_manifest.txt>`

## Entrada

- `--label` es obligatoria al procesar.
- La entrada puede ser una carpeta o un archivo `.txt`.
- Si la entrada es un `.txt`, cada linea valida se interpreta como una carpeta.
- Las lineas vacias y las que empiezan con `#` se ignoran.
- Si una linea no se puede resolver o no apunta a una carpeta valida, se informa con numero de linea y se siguen procesando las validas.

## Respaldo y restauracion

- Cada corrida crea un respaldo dentro de `he1_respaldo` usando la etiqueta indicada.
- El respaldo guarda el `manifest.txt` con la relacion entre originales y copias.
- `--restore` recupera los originales desde ese respaldo y elimina los PDFs generados por la corrida.
- La restauracion tambien limpia marcas temporales y evidencias auxiliares asociadas a la corrida.

## Archivo de evidencia

- El programa deja un `Cambios.txt` con el detalle de la ejecucion.
- Si la entrada es una carpeta, el log queda en la raiz del proyecto.
- Si la entrada es un `.txt`, el log queda en la raiz del proyecto.

## Regla de nombres

Los canonicos validos se definen en `src/main.rs` dentro de `VALID_NAMES`.

Un archivo pertenece a un grupo canonico cuando:

1. El nombre base coincide exactamente con el canonico, por ejemplo `PI.pdf`.
2. El nombre base empieza con el canonico y luego trae un sufijo permitido con `_` o `-`, por ejemplo:
   - `PI_01.pdf`
   - `PI-01.pdf`
   - `AES_extra.pdf`
   - `AES-extra.pdf`
3. El nombre se normaliza antes de comparar. La normalizacion corta el nombre al encontrar espacios, parentesis, corchetes, llaves o puntos.
4. Si el nombre normalizado termina en `_` o `-`, esos separadores finales se eliminan antes de comparar.
5. La variante especial `RDH` se trata como `RHD`.
6. La comparacion ignora mayusculas/minusculas, por eso `RHd-copia_extra.PDF` puede entrar como `RHD`.

## Ejemplos validos

- `PI.pdf`
- `PI_01.pdf`
- `PI-01.pdf`
- `PI_extra.pdf`
- `PI-extra.pdf`
- `AES (copia).pdf` puede normalizarse a `AES.pdf`
- `PI.copia.pdf` puede normalizarse a `PI.pdf`
- `RHd-copia_extra.PDF` puede entrar como `RHD.pdf`

## Ejemplos no validos

- `PI13.pdf`
- `AES4545.pdf`
- `PIx.pdf`
- `AESx.pdf`
- `PI+1.pdf`

Estos no son validos porque no usan `_` ni `-` como separador despues del canonico.

## Verificacion operativa

Cuando el grupo se consolida:

- se cuentan las paginas de cada PDF fuente;
- se genera una salida unica;
- se valida que la suma de paginas de entrada coincida con la salida;
- se valida que el PDF final no quede vacio;
- solo si pasa la validacion se eliminan los fuentes del grupo;
- si falla la corrida, se usa el respaldo de esa ejecucion para restaurar.
