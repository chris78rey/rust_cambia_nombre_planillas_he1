# Regla de unificación de PDFs

Este documento define la regla actual del script `he1-unificar-pdfs`.

Ver también:

- [Lecciones aprendidas](./LECCIONES_APRENDIDAS.md)

## Objetivo

Tomar PDFs dentro de una carpeta, identificar nombres canónicos válidos y consolidar sus variantes en un solo archivo final con nombre canónico.

## Nombres canónicos válidos

Los nombres canónicos aceptados son:

`PI`, `CC`, `CV`, `AES`, `053`, `006`, `007`, `017`, `018`, `018A`, `113`, `114`, `115`, `ORS`, `002`, `010A`, `010B`, `012A`, `012B`, `033`, `013A`, `013B`, `PTR`, `RTR`, `08`, `008`, `FSCS`, `FSICS`, `FRDCS`, `ANX2`, `HR`, `RHD`, `IMT`, `CEC`, `RAD`, `ITS`, `RVD`, `119`

## Regla principal

Para cada nombre canónico:

1. Se busca el archivo exacto `CANONICO.pdf`.
2. Se buscan variantes con sufijo numérico separado por guion bajo:
   - `CANONICO_1.pdf`
   - `CANONICO_01.pdf`
   - `CANONICO_2.pdf`
   - `CANONICO_999.pdf`
   - con la regla actual, cualquier texto después de `_` es válido, por ejemplo:
     - `CANONICO_copia.pdf`
     - `CANONICO_01_extra.pdf`
     - `CANONICO_abc.pdf`
3. Si existe solo una variante y no existe el archivo canónico exacto, la variante se renombra a `CANONICO.pdf`.
4. Si existe el archivo canónico exacto más una o más variantes, todos esos archivos se unen y el resultado final queda como `CANONICO.pdf`.
5. Después de una unión exitosa, los PDFs fuente del grupo se eliminan.

## Extensión

La salida final siempre usa la extensión `.pdf` en minúsculas.

Ejemplos:

- `RDH.PdF` termina como `RHD.pdf`
- `PI_01.PDF` termina como `PI.pdf`

## Casos permitidos

- `PI.pdf`
- `PI_1.pdf`
- `PI_01.pdf`
- `PI_copia.pdf`
- `PI_01_extra.pdf`
- `CC.pdf`
- `CC_2.pdf`
- `008_12.pdf`

## Casos no permitidos

No se consideran variantes válidas:

- `AES (copia).pdf`
- `012abc.pdf`
- `CC-1.pdf`

`PI13.pdf` sigue sin entrar porque no tiene el separador `_`.

## Casos especiales

- `RDH.PdF` se corrige como `RHD.pdf` por una regla explícita de typo conocido.
- `119a.pdf` no entra en la regla actual de variante permitida porque solo se aceptan sufijos numéricos separados por `_`.

## Verificación

Antes de aceptar una unión, el script compara:

- cantidad de páginas de la fuente,
- cantidad de páginas del resultado,
- tamaño en bytes del resultado.

Si la verificación falla, el proceso se aborta para ese grupo.

## Registro

El script deja un `Cambios.txt` en la carpeta raíz procesada con:

- fecha de inicio y fin de corrida,
- carpeta analizada,
- archivos detectados,
- antes y después por canónico,
- salida final,
- verificación,
- archivos eliminados,
- casos ignorados o no clasificados.

## Nota operativa

Si vas a retomar el proyecto después de varios meses, revisa también:

- [Lecciones aprendidas](./LECCIONES_APRENDIDAS.md)
