# Guía para levantar y usar el sistema

Esta guía explica, en pasos simples, cómo levantar el proyecto y cómo usarlo sin conocer el código.

El sistema tiene dos partes:

- **Rust**: ejecuta el proceso principal de PDFs y el bot de Telegram.
- **Go**: llena el archivo `fuentes_txt/PATH_DIRECTORIOS.txt` consultando Oracle y SQLite.

## 1. Qué hace el sistema

El flujo normal es este:

1. Se consulta Oracle.
2. Se cruza la información con SQLite.
3. Se genera `fuentes_txt/PATH_DIRECTORIOS.txt`.
4. Se procesa cada carpeta listada en ese archivo.
5. Se crean respaldos en `he1_respaldo/`.
6. El bot de Telegram permite lanzar el proceso sin escribir comandos largos.

## 2. Requisitos

Antes de usarlo, la máquina debe tener:

- acceso al repositorio del proyecto
- `Rust` instalado
- `Go` instalado
- acceso a Oracle
- acceso al archivo SQLite de trabajo
- un bot de Telegram creado

## 3. Ubicación del proyecto

En este entorno el proyecto está en:

```bash
/data_nuevo/rust_cambia_nombre_planillas_he1
```

Trabaja siempre desde esa carpeta.

## 4. Archivos importantes

Los archivos que más se usan son:

- `.env` o `.env.ubuntu22.example`
- `fuentes_txt/PATH_DIRECTORIOS.txt`
- `he1_respaldo/`
- `go_aplicacion/cmd/path_directorios_fill/main.go`
- `src/main.rs`
- `src/telegram.rs`

## 5. Configuración básica

### 5.1 Crear el `.env`

Si no existe, copia el ejemplo:

```bash
cp .env.ubuntu22.example .env
```

Luego revisa estas variables:

- `TELEGRAM_BOT_TOKEN`
- `TELEGRAM_CHAT_ID`
- `HE1_INPUT`
- `HE1_LABEL`
- `HE1_TARGET`
- `ORACLE_USER`
- `ORACLE_PASSWORD`
- `ORACLE_SCHEMA`
- `ORACLE_TABLE`
- `ORACLE_ENDPOINTS`
- `SQLITE_DSN`

En este proyecto, para el llenado de `PATH_DIRECTORIOS.txt`, la SQLite correcta es:

```bash
file:/data_nuevo/repo_grande/data/folders.sqlite
```

## 6. Levantar el bot de Telegram

### 6.1 Compilar

Desde la raíz del proyecto:

```bash
cargo build --bin rust_cambia_nombre_planillas_he1
```

### 6.2 Ejecutar el bot

Luego ejecuta:

```bash
./target/debug/rust_cambia_nombre_planillas_he1 --telegram
```

Si todo está bien, verás un mensaje parecido a:

```text
telegram activo: chat_id=..., input por defecto=...
```

### 6.3 Qué significa “levantar el servicio”

Levantar el servicio significa dejar el bot corriendo para que escuche mensajes de Telegram.

Si cierras esa terminal, el bot se detiene.

## 7. Cómo usar el bot

Abre Telegram y escribe:

```text
/help
```

Ahí verás los botones y los comandos disponibles.

### 7.1 Botón `FP`

`FP` sirve para llenar `fuentes_txt/PATH_DIRECTORIOS.txt`.

Flujo:

1. Toca `FP`.
2. Elige el campo Oracle.
3. Escribe el valor.
4. El bot llena el archivo `PATH_DIRECTORIOS.txt`.

Campos que puedes usar:

- `DIG_ID_GENERACION`
- `DIG_ID_TRAMITE`
- `DIG_TRAMITE`

Importante:

- `DIG_TRAMITE` y `DIG_ID_TRAMITE` deben ser numéricos.
- `DIG_PLANILLADO` ya está fijo en `'S'` para el cruce.

### 7.2 Procesar PDFs

Para procesar, puedes usar:

- `/process`
- `/process_report`

El bot te pedirá la etiqueta.

Ejemplo:

```text
PATH_DIRECTORIOS
```

### 7.3 Generar reporte

Puedes usar:

- `/report`

### 7.4 Restaurar

Puedes usar:

- `/restore`

## 8. Qué pasa cuando se procesa

Cuando el sistema procesa una carpeta:

- crea un respaldo en `he1_respaldo/`
- genera `manifest.txt`
- deja `Cambios.txt`
- marca la carpeta con `.he1_procesado`
- consolida o renombra PDFs según la regla

## 9. Área de trabajo temporal

El sistema usa un área de trabajo separada para archivos temporales:

```bash
he1_respaldo/area_trabajo/
```

Eso evita ensuciar la carpeta original con archivos temporales.

## 10. Qué no debes tocar

Si no conoces el sistema, evita cambiar esto sin revisar:

- `src/main.rs`
- `src/telegram.rs`
- `go_aplicacion/cmd/path_directorios_fill/main.go`
- `fuentes_txt/PATH_DIRECTORIOS.txt` mientras se esté procesando

## 11. Resumen rápido

Si solo quieres usar el sistema, haz esto:

1. Levanta el bot con `./target/debug/rust_cambia_nombre_planillas_he1 --telegram`
2. En Telegram manda `/help`
3. Usa `FP` para llenar `PATH_DIRECTORIOS.txt`
4. Usa `/process` o `/process_report` con la etiqueta

Si quieres revertir una corrida, usa `/restore`.

