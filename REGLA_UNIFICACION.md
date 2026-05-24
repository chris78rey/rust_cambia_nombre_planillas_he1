# Regla de unificacion y uso

Este es el unico documento de ayuda del proyecto. Resume las reglas que aplica `src/main.rs`.

## Comandos

- `he1-unificar-pdfs --label <etiqueta> <ruta.txt | carpeta>`
- `he1-unificar-pdfs --restore <etiqueta | ruta_respaldo_o_manifest.txt>`
- `he1-unificar-pdfs --report <etiqueta | ruta_respaldo_o_manifest.txt>`

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

## Ejecucion con Docker Desktop

Para probar el proceso en Linux sin VPS:

1. Copia `.env.example` a `.env`.
2. Ajusta `HOST_REPO` para que apunte al repo en tu equipo.
3. Ajusta `HE1_MODE`:
   - `process` para procesar
   - `process_report` para procesar y luego generar el HTML en una sola corrida
   - `restore` para revertir
   - `report` para generar el HTML
4. Usa `docker compose up --build`.

Ejemplos:

- Procesar un archivo de directorios:
  - `HE1_MODE=process`
  - `HE1_LABEL=L112-L114`
  - `HE1_INPUT=/work/fuentes_txt/PATH_DIRECTORIOS_1.txt`
- Procesar y generar HTML en una sola corrida:
  - `HE1_MODE=process_report`
  - `HE1_LABEL=L112-L114`
  - `HE1_INPUT=/work/fuentes_txt/PATH_DIRECTORIOS_1.txt`
- Restaurar:
  - `HE1_MODE=restore`
  - `HE1_TARGET=L112-L114`
- Generar HTML:
  - `HE1_MODE=report`
  - `HE1_TARGET=L112-L114`

En `process_report`, el contenedor ejecuta primero `process` y luego `report` usando la misma etiqueta. El HTML queda en la carpeta del respaldo de esa corrida.

Dentro del contenedor, las rutas deben ser Linux. El repo del host se monta en `/work`.

## Ejecucion nativa en Ubuntu 22

Cuando ya no uses Docker, el flujo es mas simple:

1. Copia `.env.ubuntu22.example` a `.env`.
2. Ajusta `TELEGRAM_BOT_TOKEN` y `TELEGRAM_CHAT_ID`.
3. Ajusta `HE1_INPUT` a la ruta real de tu repo en Ubuntu.
4. Compila con `cargo build --release`.
5. Ejecuta el binario con `HE1_MODE=telegram`, `process`, `process_report`, `restore` o `report` segun necesites.

Ejemplos:

- Procesar desde Telegram:
  - `HE1_MODE=telegram`
  - `HE1_LABEL=L112-L114`
  - `HE1_INPUT=/home/usuario/rust_cambia_nombre_planillas_he1/fuentes_txt/PATH_DIRECTORIOS.txt`
- Procesar y generar HTML:
  - `HE1_MODE=process_report`
  - `HE1_LABEL=L112-L114`
  - `HE1_INPUT=/home/usuario/rust_cambia_nombre_planillas_he1/fuentes_txt/PATH_DIRECTORIOS.txt`
- Restaurar:
  - `HE1_MODE=restore`
  - `HE1_TARGET=L112-L114`
- Generar HTML:
  - `HE1_MODE=report`
  - `HE1_TARGET=L112-L114`

En Ubuntu ya no necesitas `HOST_REPO` ni `WORKDIR` si corres el binario de forma nativa.

## Telegram local sin VPS

El bot corre en el mismo equipo y usa `long polling`. No necesita IP publica ni webhook.

Variables requeridas:

- `TELEGRAM_BOT_TOKEN`
- `TELEGRAM_CHAT_ID`
- `HE1_MODE=telegram`
- `HE1_INPUT` como entrada por defecto si usas `/process <etiqueta>` sin ruta extra

Comandos:

- `/process <etiqueta> [ruta_input]`
- `/process_report <etiqueta> [ruta_input]`
- `/restore <etiqueta | ruta_manifest_o_respaldo>`
- `/report <etiqueta | ruta_manifest_o_respaldo>`

Flujo tipico:

1. Levantas el contenedor o el binario con `HE1_MODE=telegram`.
2. Desde el celular envias `/process L112-L114`.
3. Si quieres el HTML en la misma corrida, usas `/process_report L112-L114`.
4. Para revertir, usas `/restore L112-L114`.
5. Para volver a generar el HTML, usas `/report L112-L114`.

El bot solo responde al `TELEGRAM_CHAT_ID` configurado y devuelve el HTML como adjunto cuando aplica.

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
