package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/sijms/go-ora/v2"
	_ "modernc.org/sqlite"
)

const (
	itemTable   = "CD_DESCARGA_OBJECION_ITEM"
	headerTable = "CD_DESCARGA_OBJECION"
)

type config struct {
	schema      string
	user        string
	password    string
	port        int
	endpoints   []endpoint
	timeout     time.Duration
	sqliteDSN   string
	sqliteTable string
	sqliteName  string
	sqlitePath  string
	auditSQLite string
}

type endpoint struct {
	host    string
	service string
}

type planItem struct {
	IDDescarga  string   `json:"id_descarga"`
	Tramite     string   `json:"tramite"`
	NombrePDF   string   `json:"nombre_pdf"`
	FolderPaths []string `json:"folder_paths"`
}

type planResponse struct {
	Label string     `json:"label"`
	Items []planItem `json:"items"`
}

type changeGroup struct {
	IDDescarga    string   `json:"id_descarga"`
	Tramite       string   `json:"tramite"`
	Base          string   `json:"base"`
	CanonicalName string   `json:"canonical_name"`
	PartNames     []string `json:"part_names"`
	OutputPath    string   `json:"output_path"`
	TotalPages    uint32   `json:"total_pages"`
}

type applyRequest struct {
	Label  string        `json:"label"`
	Groups []changeGroup `json:"groups"`
}

type applyResponse struct {
	Status           string `json:"status"`
	Message          string `json:"message,omitempty"`
	GroupsUpdated    int    `json:"groups_updated"`
	ItemsDeleted     int64  `json:"items_deleted"`
	DownloadsUpdated int    `json:"downloads_updated"`
}

type lockedItem struct {
	rowID string
	name  string
}

func main() {
	action := flag.String("action", "", "plan o apply")
	label := flag.String("label", "", "etiqueta de objeción")
	flag.Parse()

	if *label == "" {
		fatal(errors.New("falta --label"))
	}
	cfg, err := loadConfig()
	if err != nil {
		fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), cfg.timeout)
	defer cancel()

	db, _, err := connectOracle(ctx, cfg)
	if err != nil {
		fatal(err)
	}
	defer db.Close()

	switch *action {
	case "plan":
		result, err := buildPlan(ctx, db, cfg, *label)
		if err != nil {
			fatal(err)
		}
		if err := json.NewEncoder(os.Stdout).Encode(result); err != nil {
			fatal(err)
		}
	case "apply":
		var request applyRequest
		if err := json.NewDecoder(io.LimitReader(os.Stdin, 8<<20)).Decode(&request); err != nil {
			fatal(fmt.Errorf("leer solicitud de actualización: %w", err))
		}
		if request.Label != *label {
			fatal(errors.New("la etiqueta de la solicitud no coincide con --label"))
		}
		if len(request.Groups) == 0 {
			fatal(errors.New("no hay grupos para actualizar"))
		}
		result, err := applyChanges(ctx, db, cfg, request)
		if err != nil {
			fatal(err)
		}
		if err := json.NewEncoder(os.Stdout).Encode(result); err != nil {
			fatal(err)
		}
	default:
		fatal(errors.New("usa --action plan o --action apply"))
	}
}

func loadConfig() (config, error) {
	var cfg config
	cfg.schema = envOr("ORACLE_SCHEMA", "DIGITALIZACION")
	cfg.user = os.Getenv("ORACLE_USER")
	cfg.password = os.Getenv("ORACLE_PASSWORD")
	cfg.port = envInt("HE1_OBJECTION_ORACLE_PORT", envInt("ORACLE_PORT", 1521))
	cfg.timeout = envDuration("ORACLE_TIMEOUT", 2*time.Minute)
	cfg.sqliteDSN = envOr("SQLITE_DSN", "file:/data_nuevo/repo_grande/data/folders.sqlite")
	cfg.sqliteTable = envOr("SQLITE_TABLE", "folders")
	cfg.sqliteName = envOr("SQLITE_NAME_COL", "name")
	cfg.sqlitePath = envOr("SQLITE_PATH_COL", "path")
	cfg.auditSQLite = envOr("HE1_OBJECTION_AUDIT_SQLITE", "uploads_audit.sqlite")

	if cfg.user == "" || cfg.password == "" {
		return config{}, errors.New("faltan ORACLE_USER u ORACLE_PASSWORD en el entorno")
	}
	for _, value := range []string{cfg.schema, cfg.sqliteTable, cfg.sqliteName, cfg.sqlitePath} {
		if err := validateIdent(value); err != nil {
			return config{}, err
		}
	}

	for _, raw := range strings.Split(envOr("ORACLE_ENDPOINTS", "172.16.60.20:PRDSGH1,172.16.60.20:PRDSGH2,172.16.60.21:PRDSGH"), ",") {
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		host, service, ok := strings.Cut(raw, ":")
		if !ok || host == "" || service == "" {
			return config{}, fmt.Errorf("endpoint Oracle inválido: %q", raw)
		}
		cfg.endpoints = append(cfg.endpoints, endpoint{host: host, service: service})
	}
	if len(cfg.endpoints) == 0 {
		return config{}, errors.New("no hay endpoints Oracle configurados")
	}
	return cfg, nil
}

func connectOracle(ctx context.Context, cfg config) (*sql.DB, endpoint, error) {
	var lastErr error
	for _, ep := range cfg.endpoints {
		conn := go_ora.BuildUrl(ep.host, cfg.port, ep.service, cfg.user, cfg.password, nil)
		db, err := sql.Open("oracle", conn)
		if err != nil {
			lastErr = err
			continue
		}
		pingCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
		err = db.PingContext(pingCtx)
		cancel()
		if err != nil {
			lastErr = err
			_ = db.Close()
			continue
		}
		return db, ep, nil
	}
	return nil, endpoint{}, fmt.Errorf("no se pudo conectar a Oracle: %w", lastErr)
}

func buildPlan(ctx context.Context, oracleDB *sql.DB, cfg config, label string) (planResponse, error) {
	query := fmt.Sprintf(`SELECT TO_CHAR(d.ID_DESCARGA), TO_CHAR(i.DIG_TRAMITE), TRIM(i.NOMBRE_PDF)
FROM %s.%s d
JOIN %s.%s i ON i.ID_DESCARGA = d.ID_DESCARGA
WHERE d.LABEL_OBJECION = :1
ORDER BY TO_CHAR(d.ID_DESCARGA), TO_CHAR(i.DIG_TRAMITE), TRIM(i.NOMBRE_PDF)`,
		oracleIdent(cfg.schema), headerTable, oracleIdent(cfg.schema), itemTable)
	rows, err := oracleDB.QueryContext(ctx, query, label)
	if err != nil {
		return planResponse{}, fmt.Errorf("consultar ítems de objeción: %w", err)
	}
	defer rows.Close()

	var items []planItem
	tramites := map[string]struct{}{}
	for rows.Next() {
		var item planItem
		var id, tramite, name sql.NullString
		if err := rows.Scan(&id, &tramite, &name); err != nil {
			return planResponse{}, err
		}
		item.IDDescarga = strings.TrimSpace(id.String)
		item.Tramite = strings.TrimSpace(tramite.String)
		item.NombrePDF = strings.TrimSpace(name.String)
		if !id.Valid || item.IDDescarga == "" || !tramite.Valid || item.Tramite == "" || !name.Valid || item.NombrePDF == "" {
			continue
		}
		items = append(items, item)
		tramites[item.Tramite] = struct{}{}
	}
	if err := rows.Err(); err != nil {
		return planResponse{}, err
	}
	if len(items) == 0 {
		return planResponse{}, fmt.Errorf("no hay ítems para la etiqueta %q", label)
	}

	sqliteDB, err := sql.Open("sqlite", cfg.sqliteDSN)
	if err != nil {
		return planResponse{}, fmt.Errorf("abrir SQLite de carpetas: %w", err)
	}
	defer sqliteDB.Close()
	if err := sqliteDB.PingContext(ctx); err != nil {
		return planResponse{}, fmt.Errorf("conectar a SQLite de carpetas: %w", err)
	}
	pathsByTramite := make(map[string][]string, len(tramites))
	for tramite := range tramites {
		paths, err := lookupFolderPaths(ctx, sqliteDB, cfg, tramite)
		if err != nil {
			return planResponse{}, err
		}
		var existing []string
		for _, path := range paths {
			info, err := os.Stat(path)
			if err == nil && info.IsDir() {
				existing = append(existing, path)
			}
		}
		if len(existing) == 0 {
			return planResponse{}, fmt.Errorf("la ruta de SQLite para el trámite %q no es una carpeta disponible", tramite)
		}
		pathsByTramite[tramite] = existing
	}
	for i := range items {
		items[i].FolderPaths = pathsByTramite[items[i].Tramite]
	}
	sort.Slice(items, func(i, j int) bool {
		if items[i].IDDescarga != items[j].IDDescarga {
			return items[i].IDDescarga < items[j].IDDescarga
		}
		if items[i].Tramite != items[j].Tramite {
			return items[i].Tramite < items[j].Tramite
		}
		return items[i].NombrePDF < items[j].NombrePDF
	})
	return planResponse{Label: label, Items: items}, nil
}

func lookupFolderPaths(ctx context.Context, db *sql.DB, cfg config, tramite string) ([]string, error) {
	query := fmt.Sprintf(`SELECT DISTINCT %s FROM %s WHERE %s = ? ORDER BY %s`,
		sqliteIdent(cfg.sqlitePath), sqliteIdent(cfg.sqliteTable), sqliteIdent(cfg.sqliteName), sqliteIdent(cfg.sqlitePath))
	rows, err := db.QueryContext(ctx, query, tramite)
	if err != nil {
		return nil, fmt.Errorf("buscar ruta del trámite %q en SQLite: %w", tramite, err)
	}
	defer rows.Close()
	var paths []string
	for rows.Next() {
		var path sql.NullString
		if err := rows.Scan(&path); err != nil {
			return nil, err
		}
		if path.Valid && strings.TrimSpace(path.String) != "" {
			paths = append(paths, filepath.Clean(strings.TrimSpace(path.String)))
		}
	}
	return paths, rows.Err()
}

func applyChanges(ctx context.Context, oracleDB *sql.DB, cfg config, request applyRequest) (applyResponse, error) {
	for _, group := range request.Groups {
		if !validBase(group.Base) || group.CanonicalName != group.Base+".pdf" || group.IDDescarga == "" || group.Tramite == "" || len(group.PartNames) < 2 {
			return applyResponse{}, errors.New("grupo de actualización inválido")
		}
		seen := map[string]struct{}{}
		for _, name := range group.PartNames {
			if !isPartForBase(name, group.Base) {
				return applyResponse{}, fmt.Errorf("nombre de parte inválido para %s: %q", group.Base, name)
			}
			seen[normalName(name)] = struct{}{}
		}
		if len(seen) < 2 {
			return applyResponse{}, errors.New("el grupo debe contener al menos dos nombres de parte distintos")
		}
	}
	operationID := fmt.Sprintf("%s-%d", request.Label, time.Now().UTC().UnixNano())
	auditDB, err := openAuditDB(cfg.auditSQLite)
	if err != nil {
		return applyResponse{}, err
	}
	defer auditDB.Close()
	if err := writeAuditRows(ctx, auditDB, request, operationID, "pending", ""); err != nil {
		return applyResponse{}, fmt.Errorf("registrar auditoría local: %w", err)
	}

	tx, err := oracleDB.BeginTx(ctx, nil)
	if err != nil {
		_ = writeAuditRows(ctx, auditDB, request, operationID, "failed", err.Error())
		return applyResponse{}, err
	}
	result := applyResponse{Status: "committed"}
	affectedDownloads := map[string]struct{}{}
	for _, group := range request.Groups {
		deleted, changed, err := applyOneGroup(ctx, tx, cfg, group)
		if err != nil {
			_ = tx.Rollback()
			_ = writeAuditRows(ctx, auditDB, request, operationID, "failed", err.Error())
			return applyResponse{}, err
		}
		if changed {
			result.GroupsUpdated++
			result.ItemsDeleted += deleted
			affectedDownloads[group.IDDescarga] = struct{}{}
		}
	}
	for id := range affectedDownloads {
		update := fmt.Sprintf(`UPDATE %s.%s d
SET TOTAL_PDFS = (SELECT COUNT(*) FROM %s.%s i WHERE i.ID_DESCARGA = d.ID_DESCARGA)
WHERE d.ID_DESCARGA = :1 AND d.LABEL_OBJECION = :2`,
			oracleIdent(cfg.schema), headerTable, oracleIdent(cfg.schema), itemTable)
		res, err := tx.ExecContext(ctx, update, id, request.Label)
		if err != nil {
			_ = tx.Rollback()
			_ = writeAuditRows(ctx, auditDB, request, operationID, "failed", err.Error())
			return applyResponse{}, fmt.Errorf("recalcular TOTAL_PDFS de %s: %w", id, err)
		}
		n, err := res.RowsAffected()
		if err != nil || n != 1 {
			_ = tx.Rollback()
			message := fmt.Sprintf("se esperaba actualizar una cabecera para ID_DESCARGA=%s; filas=%d", id, n)
			_ = writeAuditRows(ctx, auditDB, request, operationID, "failed", message)
			return applyResponse{}, errors.New(message)
		}
		result.DownloadsUpdated++
	}
	if err := tx.Commit(); err != nil {
		_ = writeAuditRows(ctx, auditDB, request, operationID, "commit_uncertain", err.Error())
		return applyResponse{Status: "commit_uncertain", Message: err.Error()}, nil
	}
	if err := writeAuditRows(ctx, auditDB, request, operationID, "oracle_committed", ""); err != nil {
		fmt.Fprintf(os.Stderr, "advertencia: Oracle confirmó cambios, pero no se pudo actualizar la auditoría SQLite: %v\n", err)
	}
	return result, nil
}

func applyOneGroup(ctx context.Context, tx *sql.Tx, cfg config, group changeGroup) (int64, bool, error) {
	query := fmt.Sprintf(`SELECT ROWIDTOCHAR(i.ROWID), TRIM(i.NOMBRE_PDF)
FROM %s.%s i
WHERE i.ID_DESCARGA = :1 AND TRIM(TO_CHAR(i.DIG_TRAMITE)) = :2
FOR UPDATE`, oracleIdent(cfg.schema), itemTable)
	rows, err := tx.QueryContext(ctx, query, group.IDDescarga, group.Tramite)
	if err != nil {
		return 0, false, err
	}
	var all []lockedItem
	for rows.Next() {
		var row lockedItem
		if err := rows.Scan(&row.rowID, &row.name); err != nil {
			_ = rows.Close()
			return 0, false, err
		}
		row.rowID = strings.TrimSpace(row.rowID)
		row.name = strings.TrimSpace(row.name)
		all = append(all, row)
	}
	rowsErr := rows.Err()
	_ = rows.Close()
	if rowsErr != nil {
		return 0, false, rowsErr
	}

	parts := make(map[string]struct{}, len(group.PartNames))
	for _, name := range group.PartNames {
		parts[normalName(name)] = struct{}{}
	}
	var matched, canonicals, splitRows []lockedItem
	foundParts := map[string]struct{}{}
	for _, row := range all {
		name := normalName(row.name)
		if name == normalName(group.CanonicalName) || (group.Base == "RHD" && name == "RDH.PDF") {
			canonicals = append(canonicals, row)
			matched = append(matched, row)
		} else if _, ok := parts[name]; ok {
			splitRows = append(splitRows, row)
			matched = append(matched, row)
			foundParts[name] = struct{}{}
		}
	}
	if len(splitRows) == 0 && len(canonicals) > 0 {
		return 0, false, nil
	}
	if len(foundParts) != len(parts) {
		return 0, false, fmt.Errorf("los ítems Oracle cambiaron desde la planificación para %s/%s", group.Tramite, group.Base)
	}
	if len(matched) == 0 {
		return 0, false, fmt.Errorf("no se encontraron filas Oracle para %s/%s", group.Tramite, group.Base)
	}
	sort.Slice(matched, func(i, j int) bool { return matched[i].rowID < matched[j].rowID })
	keep := matched[0]
	if len(canonicals) > 0 {
		sort.Slice(canonicals, func(i, j int) bool { return canonicals[i].rowID < canonicals[j].rowID })
		keep = canonicals[0]
	}
	if keep.name != group.CanonicalName {
		update := fmt.Sprintf(`UPDATE %s.%s SET NOMBRE_PDF = :1 WHERE ROWID = CHARTOROWID(:2)`, oracleIdent(cfg.schema), itemTable)
		res, err := tx.ExecContext(ctx, update, group.CanonicalName, keep.rowID)
		if err != nil {
			return 0, false, err
		}
		if n, err := res.RowsAffected(); err != nil || n != 1 {
			return 0, false, fmt.Errorf("no se actualizó exactamente una fila para %s/%s", group.Tramite, group.Base)
		}
	}
	var deleted int64
	remove := fmt.Sprintf(`DELETE FROM %s.%s WHERE ROWID = CHARTOROWID(:1)`, oracleIdent(cfg.schema), itemTable)
	for _, row := range matched {
		if row.rowID == keep.rowID {
			continue
		}
		res, err := tx.ExecContext(ctx, remove, row.rowID)
		if err != nil {
			return 0, false, err
		}
		n, err := res.RowsAffected()
		if err != nil || n != 1 {
			return 0, false, fmt.Errorf("no se pudo eliminar exactamente una fila duplicada para %s/%s", group.Tramite, group.Base)
		}
		deleted += n
	}
	return deleted, true, nil
}

func openAuditDB(path string) (*sql.DB, error) {
	if path == "" {
		return nil, errors.New("HE1_OBJECTION_AUDIT_SQLITE no puede estar vacío")
	}
	if path != ":memory:" && !strings.HasPrefix(path, "file:") {
		abs, err := filepath.Abs(path)
		if err != nil {
			return nil, err
		}
		if err := os.MkdirAll(filepath.Dir(abs), 0o755); err != nil {
			return nil, err
		}
		path = abs
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	_, err = db.Exec(`CREATE TABLE IF NOT EXISTS objection_consolidation_audit (
operation_id TEXT NOT NULL,
label_objecion TEXT NOT NULL,
id_descarga TEXT NOT NULL,
tramite TEXT NOT NULL,
action TEXT NOT NULL,
documento TEXT NOT NULL,
archivos_origen TEXT NOT NULL,
archivo_destino TEXT NOT NULL,
paginas_totales INTEGER NOT NULL,
estado TEXT NOT NULL,
detalle TEXT NOT NULL,
updated_at TEXT NOT NULL,
PRIMARY KEY (operation_id, id_descarga, tramite, documento)
)`)
	if err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("crear tabla de auditoría SQLite: %w", err)
	}
	return db, nil
}

func writeAuditRows(ctx context.Context, db *sql.DB, request applyRequest, operationID, status, detail string) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	statement, err := tx.PrepareContext(ctx, `INSERT INTO objection_consolidation_audit
(operation_id,label_objecion,id_descarga,tramite,action,documento,archivos_origen,archivo_destino,paginas_totales,estado,detalle,updated_at)
VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
ON CONFLICT(operation_id,id_descarga,tramite,documento) DO UPDATE SET
action=excluded.action,
archivos_origen=excluded.archivos_origen, archivo_destino=excluded.archivo_destino,
paginas_totales=excluded.paginas_totales, estado=excluded.estado, detalle=excluded.detalle,
updated_at=excluded.updated_at`)
	if err != nil {
		_ = tx.Rollback()
		return err
	}
	defer statement.Close()
	for _, group := range request.Groups {
		origins, err := json.Marshal(group.PartNames)
		if err != nil {
			_ = tx.Rollback()
			return err
		}
		if _, err := statement.ExecContext(ctx, operationID, request.Label, group.IDDescarga, group.Tramite, "consolidacion_objecion", group.Base, string(origins), group.CanonicalName, group.TotalPages, status, detail, time.Now().UTC().Format(time.RFC3339Nano)); err != nil {
			_ = tx.Rollback()
			return err
		}
	}
	return tx.Commit()
}

func validBase(value string) bool {
	for _, allowed := range []string{"PI", "CC", "CV", "AES", "053", "006", "007", "017", "018", "018A", "113", "114", "115", "ORS", "002", "010A", "010B", "012A", "012B", "033", "013A", "013B", "PTR", "RTR", "08", "008", "FSCS", "FSICS", "FRDCS", "ANX2", "HR", "RHD", "IMT", "CEC", "RAD", "ITS", "RVD", "119"} {
		if strings.EqualFold(value, allowed) {
			return true
		}
	}
	return false
}

func isPartForBase(name, base string) bool {
	name = filepath.Base(strings.TrimSpace(name))
	if !strings.EqualFold(filepath.Ext(name), ".pdf") {
		return false
	}
	stem := strings.TrimSuffix(name, filepath.Ext(name))
	normalized := normalizeObjectionStem(stem)
	base = strings.ToUpper(base)
	if normalized == base {
		return !strings.EqualFold(strings.TrimSpace(stem), base)
	}
	if base == "RHD" && normalized == "RDH" {
		return !strings.EqualFold(strings.TrimSpace(stem), "RDH")
	}
	if base == "RHD" && strings.HasPrefix(normalized, "RDH") {
		suffix := strings.TrimPrefix(normalized, "RDH")
		return (strings.HasPrefix(suffix, "_") || strings.HasPrefix(suffix, "-")) && len(strings.TrimSpace(suffix[1:])) > 0
	}
	if !strings.HasPrefix(normalized, base) {
		return false
	}
	suffix := strings.TrimPrefix(normalized, base)
	return (strings.HasPrefix(suffix, "_") || strings.HasPrefix(suffix, "-")) && len(strings.TrimSpace(suffix[1:])) > 0
}

func normalizeObjectionStem(stem string) string {
	stem = strings.TrimSpace(stem)
	if index := strings.IndexAny(stem, " ()[]{}."); index >= 0 {
		stem = stem[:index]
	}
	return strings.ToUpper(strings.TrimSpace(strings.TrimRight(stem, "_-")))
}

func normalName(value string) string { return strings.ToUpper(strings.TrimSpace(filepath.Base(value))) }

func validateIdent(value string) error {
	value = strings.TrimSpace(value)
	if value == "" {
		return errors.New("identificador SQL vacío")
	}
	for _, r := range value {
		if r == '_' || r == '$' || r == '#' || r == '.' || (r >= 'A' && r <= 'Z') || (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') {
			continue
		}
		return fmt.Errorf("identificador SQL inválido: %q", value)
	}
	return nil
}

func oracleIdent(value string) string { return strings.ToUpper(strings.TrimSpace(value)) }
func sqliteIdent(value string) string {
	return `"` + strings.ReplaceAll(strings.TrimSpace(value), `"`, `""`) + `"`
}
func envOr(key, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}
func envInt(key string, fallback int) int {
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		return fallback
	}
	n, err := strconv.Atoi(value)
	if err != nil {
		return fallback
	}
	return n
}
func envDuration(key string, fallback time.Duration) time.Duration {
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		return fallback
	}
	d, err := time.ParseDuration(value)
	if err != nil {
		return fallback
	}
	return d
}
func fatal(err error) { fmt.Fprintln(os.Stderr, err); os.Exit(1) }
