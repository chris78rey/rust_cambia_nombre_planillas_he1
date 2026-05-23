# Regla de unificacion

Este documento define la regla operativa que usa el ejecutable para agrupar y consolidar PDFs por nombre canonico.

## Canonicos validos

Los canonicos se definen en `src/main.rs` dentro de `VALID_NAMES`.

## Regla de deteccion

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
