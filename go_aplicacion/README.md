# go_aplicacion

Aplicacion CLI en Go para conectarse a una base Oracle y otra SQLite desde el mismo ejecutable.

## Que hace

- Abre conexion a Oracle.
- Abre conexion a SQLite.
- Ejecuta una consulta en cada base.
- Imprime resultados en texto o JSON.
- Puede exportar la salida de SQLite a CSV.

Si no indicas consultas, hace una inspeccion basica de tablas:

- Oracle: `SELECT table_name FROM user_tables ORDER BY table_name`
- SQLite: `SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name`

## Variables de entorno

- `ORACLE_DSN`
- `SQLITE_DSN`
- `ORACLE_QUERY`
- `SQLITE_QUERY`
- `SQLITE_EXPORT`
- `TIMEOUT`
- `JSON_OUTPUT`

Oracle es opcional. Si no defines `ORACLE_DSN`, la app solo consulta SQLite.

## Ejemplo

```bash
export ORACLE_DSN='oracle://usuario:password@host:1521/servicio'
export SQLITE_DSN='file:local.db'
export SQLITE_EXPORT='out/folders.csv'
../bin/go_aplicacion
```

Si `SQLITE_EXPORT` apunta a un archivo, el resultado de la consulta SQLite se guarda en CSV.

## Requisitos

- Go 1.26 o superior
- Acceso de red para descargar dependencias
- Para Oracle, un DSN valido y acceso al servidor
