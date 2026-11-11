#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UNIT_SRC="${ROOT_DIR}/systemd/rust_cambia_nombre_planillas_he1.service"
UNIT_DST="/etc/systemd/system/rust_cambia_nombre_planillas_he1.service"
if [[ "${EUID}" -eq 0 ]]; then
  SUDO=()
else
  SUDO=(sudo)
fi

if [[ ! -x "${ROOT_DIR}/target/release/rust_cambia_nombre_planillas_he1" ]]; then
  echo "Falta el binario release. Compila primero con:" >&2
  echo "  cargo build --release" >&2
  exit 1
fi

"${SUDO[@]}" install -Dm644 "${UNIT_SRC}" "${UNIT_DST}"
"${SUDO[@]}" systemctl daemon-reload
"${SUDO[@]}" systemctl enable --now rust_cambia_nombre_planillas_he1.service

echo "Servicio instalado y habilitado: rust_cambia_nombre_planillas_he1.service"
echo "Estado actual:"
"${SUDO[@]}" systemctl status rust_cambia_nombre_planillas_he1.service --no-pager -l
