# Lecciones aprendidas

Este documento resume la logica correcta del flujo de unificacion, respaldo y restauracion, y los fallos que se detectaron durante las pruebas.

## Objetivo real

Consolidar PDFs por nombre canonico, dejando un unico archivo final por grupo, sin perder paginas ni borrar el resultado final por error, y poder volver al estado previo si hace falta.

## Regla correcta actual

1. Se toma un nombre canonico valido.
2. Se aceptan como parte del grupo:
   - el archivo exacto `CANONICO.pdf`
   - variantes con `_` o `-` y texto posterior, por ejemplo:
     - `CANONICO_1.pdf`
     - `CANONICO-1.pdf`
     - `CANONICO_01.PDF`
     - `CANONICO-01.PDF`
     - `CANONICO_extra.PdF`
     - `CANONICO-extra.PdF`
     - nombres que se normalizan al canonico por espacios, parentesis, corchetes, llaves o puntos, por ejemplo `AES (copia).pdf` o `PI.copia.pdf`
     - la comparacion no distingue mayusculas/minusculas, por ejemplo `RHd-copia_extra.PDF` puede entrar como `RHD.pdf`
     - no se aceptan nombres pegados sin `_` ni `-`, como `PI13.pdf` o `AES4545.pdf`
3. Se cuentan las paginas de cada fuente antes de unir.
4. Se une el grupo en un archivo temporal.
5. Se verifica que:
   - la suma de paginas fuente coincida con la salida;
   - la salida abra correctamente;
   - la salida no quede vacia, con mas de 0 bytes.
6. Solo si la verificacion pasa:
   - se borran las fuentes del grupo;
   - se conserva el archivo final.
7. Si una carpeta falla, se restauran los archivos tocados desde el respaldo de esa corrida.
8. La entrada puede venir de una carpeta o de un `.txt` con una ruta por linea.
9. El recorrido operativo es por carpeta listada, no por recursividad sobre toda la raiz.
10. La ruta contenedora solo se usa para dejar `Cambios.txt` y `.he1_respaldo`.

## Fallos que ya ocurrieron

### 1. Sobrescritura por normalizar extensiones demasiado pronto

Se intento convertir `.PDF`, `.PdF`, etc. a `.pdf` renombrando directamente.

Problema:
- si ya existia `RHD.pdf` y tambien `RHD.PDF`, uno podia reemplazar al otro;
- eso provoco perdida de paginas.

Leccion:
- no normalizar extensiones destructivamente antes de consolidar;
- tratar las variantes de extension como equivalentes al leer, no al mover.

### 2. Borrado accidental del archivo final

En algunos grupos, el archivo canonico exacto ya existia como fuente y ademas era el destino final.

Problema:
- despues de unir, la rutina de borrado eliminaba tambien el archivo final recien creado.

Leccion:
- al borrar fuentes, excluir siempre la ruta final `output`.

### 3. Reprocesamiento de carpetas ya consolidadas

Se ejecuto mas de una vez sobre la misma carpeta.

Problema:
- se corria el riesgo de volver a tocar archivos ya consolidados.

Leccion:
- dejar una marca oculta `.he1_procesado` en cada carpeta procesada;
- si la marca existe, omitir la carpeta en futuras corridas.

### 4. Agrupamiento demasiado permisivo

Se probaron reglas que aceptaban sufijos ambiguos o texto libre sin control.

Problema:
- se mezclaban archivos que no debian entrar;
- aparecieron clasificaciones dudosas.

Leccion:
- mantener una regla simple y clara;
- documentar explicitamente que entra y que no entra;
- si una regla cambia, alinear de inmediato codigo y documentacion.

### 5. Procesar sin respaldo reversible

Se planteo la posibilidad de borrar fuentes sin guardar el estado previo.

Problema:
- si la union falla a mitad de proceso, no hay forma de reconstruir exactamente lo anterior;
- separar paginas no recupera nombres ni metadatos originales;
- un PDF unido no conserva por si solo el origen de cada pagina.

Leccion:
- antes de tocar archivos, guardar respaldo de los PDFs que realmente se van a modificar;
- registrar en un manifiesto que archivo original correspondia a cada copia;
- ofrecer una rutina de restauracion para volver al estado previo.

## Cosas que si funcionaron

- Conteo de paginas por archivo antes de unir.
- Conteo de paginas del PDF final.
- Conservacion del archivo final cuando el canonico exacto ya formaba parte del grupo.
- Registro de la corrida en `Cambios.txt`.
- Copia de prueba con carpeta `before` y `after` para comparar el resultado.
- Respaldo por corrida en `.he1_respaldo`.
- Restauracion desde `manifest.txt`.

## Recomendacion para futuras modificaciones

Antes de tocar la logica:

1. Revisar este documento.
2. Probar con una carpeta de demo.
3. Confirmar que `before` y `after` coinciden con lo esperado.
4. Verificar que el respaldo permite volver al estado previo.
5. No volver a introducir normalizaciones destructivas ni borrados sin excluir la salida final.

## Resumen ejecutivo

- Nunca renombrar un archivo sobre otro existente sin control.
- Nunca borrar el destino final.
- Nunca reprocesar una carpeta ya marcada.
- Verificar siempre paginas fuente vs salida.
- Mantener `Cambios.txt` como evidencia operativa.
- Procesar solo las carpetas indicadas en la entrada, sin recorrer recursivamente toda la raiz.
- Guardar respaldo antes de modificar si luego se quiere permitir restauracion.
