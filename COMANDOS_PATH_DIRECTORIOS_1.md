# Comandos para `PATH_DIRECTORIOS_1.txt`

## Procesar

```powershell
G:\codex_projects\rust_cambia_nombre_planillas_he1\target\debug\rust_cambia_nombre_planillas_he1.exe "G:\codex_projects\rust_cambia_nombre_planillas_he1\fuentes_txt\PATH_DIRECTORIOS_1.txt"
```

## Revertir

No se puede revertir usando solo `PATH_DIRECTORIOS_1.txt`, porque ese archivo solo lista carpetas a procesar y no guarda el estado previo.

Para deshacer una corrida hace falta el respaldo que genera el programa durante el proceso. Ese respaldo contiene el `manifest.txt` y las copias originales necesarias para restaurar.

Si quieres, te preparo una version del ejecutable para que acepte `PATH_DIRECTORIOS_1.txt` tambien en modo restauracion y busque automaticamente el respaldo asociado.
