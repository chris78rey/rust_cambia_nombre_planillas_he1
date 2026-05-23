#!/bin/sh
set -eu

if [ "$#" -gt 0 ]; then
  exec he1-unificar-pdfs "$@"
fi

case "${HE1_MODE:-process}" in
  process)
    exec he1-unificar-pdfs --label "${HE1_LABEL:?HE1_LABEL requerido}" "${HE1_INPUT:?HE1_INPUT requerido}"
    ;;
  process_report|process+report|process-report)
    label="${HE1_LABEL:?HE1_LABEL requerido}"
    input="${HE1_INPUT:?HE1_INPUT requerido}"
    report_target="${HE1_TARGET:-$label}"

    he1-unificar-pdfs --label "$label" "$input"
    he1-unificar-pdfs --report "$report_target"
    ;;
  restore)
    exec he1-unificar-pdfs --restore "${HE1_TARGET:?HE1_TARGET requerido}"
    ;;
  report)
    exec he1-unificar-pdfs --report "${HE1_TARGET:?HE1_TARGET requerido}"
    ;;
  telegram)
    exec he1-unificar-pdfs --telegram
    ;;
  *)
    echo "HE1_MODE invalido: ${HE1_MODE:-}" >&2
    exit 2
    ;;
esac
