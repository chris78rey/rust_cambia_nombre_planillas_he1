# Lecciones aprendidas

Este documento resume la lógica correcta del flujo de unificación y los fallos que se detectaron durante las pruebas.

## Objetivo real

Consolidar PDFs por nombre canónico, dejando un único archivo final por grupo, sin perder páginas ni borrar el resultado final por error.

## Regla correcta actual

1. Se toma un nombre canónico válido.
2. Se aceptan como parte del grupo:
   - el archivo exacto `CANONICO.pdf`
   - variantes con `_` y texto posterior, por ejemplo:
     - `CANONICO_1.pdf`
     - `CANONICO_01.PDF`
     - `CANONICO_extra.PdF`
3. Se cuentan las páginas de cada fuente antes de unir.
4. Se une el grupo en un archivo temporal.
5. Se verifica que:
   - la suma de páginas fuente coincida con la salida;
   - la salida abra correctamente;
   - la salida tenga extensión `.pdf` en minúsculas.
6. Solo si la verificación pasa:
   - se borran las fuentes del grupo;
   - se conserva el archivo final.

## Fallos que ya ocurrieron

### 1. Sobrescritura por normalizar extensiones demasiado pronto

Se intentó convertir `.PDF`, `.PdF`, etc. a `.pdf` renombrando directamente.

Problema:
- si ya existía `RHD.pdf` y también `RHD.PDF`, uno podía reemplazar al otro;
- eso provocó pérdida de páginas.

Lección:
- no normalizar extensiones destructivamente antes de consolidar;
- tratar las variantes de extensión como equivalentes al leer, no al mover.

### 2. Borrado accidental del archivo final

En algunos grupos, el archivo canónico exacto ya existía como fuente y además era el destino final.

Problema:
- después de unir, la rutina de borrado eliminaba también el archivo final recién creado.

Lección:
- al borrar fuentes, excluir siempre la ruta final `output`.

### 3. Reprocesamiento de carpetas ya consolidadas

Se ejecutó más de una vez sobre la misma carpeta.

Problema:
- se corría el riesgo de volver a tocar archivos ya consolidados.

Lección:
- dejar una marca oculta `.he1_procesado` en cada carpeta procesada;
- si la marca existe, omitir la carpeta en futuras corridas.

### 4. Agrupamiento demasiado permisivo

Se probaron reglas que aceptaban sufijos ambiguos o texto libre sin control.

Problema:
- se mezclaban archivos que no debían entrar;
- aparecieron clasificaciones dudosas.

Lección:
- mantener una regla simple y clara;
- documentar explícitamente qué entra y qué no entra.

## Cosas que sí funcionaron

- Conteo de páginas por archivo antes de unir.
- Conteo de páginas del PDF final.
- Conservación del archivo final cuando el canónico exacto ya formaba parte del grupo.
- Registro de la corrida en `Cambios.txt`.
- Copia de prueba con carpeta `before` y `after` para comparar el resultado.

## Recomendación para futuras modificaciones

Antes de tocar la lógica:

1. Revisar este documento.
2. Probar con una carpeta de demo.
3. Confirmar que `before` y `after` coinciden con lo esperado.
4. No volver a introducir normalizaciones destructivas ni borrados sin excluir la salida final.

## Resumen ejecutivo

- Nunca renombrar un archivo sobre otro existente sin control.
- Nunca borrar el destino final.
- Nunca reprocesar una carpeta ya marcada.
- Verificar siempre páginas fuente vs salida.
- Mantener `Cambios.txt` como evidencia operativa.

