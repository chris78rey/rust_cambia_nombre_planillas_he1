# Operacion del bot de Telegram

Fecha de referencia: 2026-07-29.

## Objetivo

Mantener el bot de Telegram de este repo corriendo de forma permanente, incluso despues de reiniciar el host.

## Lo que debe existir

- Binario release compilado en `target/release/rust_cambia_nombre_planillas_he1`
- Archivo `.env` en la raiz del repo
- Unidad de systemd instalada en `/etc/systemd/system/rust_cambia_nombre_planillas_he1.service`

## Como instalarlo

Desde la raiz del repo:

```bash
cargo build --release
sudo ./scripts/install_systemd_service.sh
```

Ese script copia la unidad, recarga systemd y deja el servicio habilitado para arranque automatico.

## Como verificar que quedo bien

```bash
systemctl status rust_cambia_nombre_planillas_he1.service
journalctl -u rust_cambia_nombre_planillas_he1.service -f
```

## Como reiniciarlo

```bash
sudo systemctl restart rust_cambia_nombre_planillas_he1.service
```

## Como apagarlo

```bash
sudo systemctl stop rust_cambia_nombre_planillas_he1.service
```

## Que hace el servicio

- Arranca `rust_cambia_nombre_planillas_he1 --telegram`
- Usa `.env` del repo
- Reinicia si cae
- Falla temprano si falta el binario release o el `.env`
- Envía aviso por Telegram cuando arranca y cuando se detiene por error repetido de polling

## Si el bot responde raro

Primero revisa:

1. `journalctl -u rust_cambia_nombre_planillas_he1.service -n 100 --no-pager`
2. Si el proceso no aparece, vuelve a arrancar con `systemctl restart`
3. Si `/start` no muestra el menu, manda `/help`

## Recordatorio

El repo ya trae la unidad y el instalador, pero la instalacion real en el host requiere privilegios de `sudo`. Si en el futuro no recuerdas nada mas, ejecuta el bloque de instalacion de arriba y luego valida con `systemctl status`.
