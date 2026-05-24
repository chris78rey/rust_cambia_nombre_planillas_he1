package main

import (
	"context"
	"database/sql"
	"encoding/csv"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/sijms/go-ora/v2"
)

type endpoint struct {
	Host    string
	Service string
}

type columnInfo struct {
	ColumnID      int
	ColumnName    string
	DataType      string
	DataLength    sql.NullInt64
	DataPrecision sql.NullInt64
	DataScale     sql.NullInt64
	Nullable      string
}

type exportConfig struct {
	User      string
	Password  string
	Schema    string
	Table     string
	ExportDir string
	Port      int
	Timeout   time.Duration
	Endpoints []endpoint
}

func main() {
	cfg, err := parseConfig()
	if err != nil {
		fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), cfg.Timeout)
	defer cancel()

	db, usedEndpoint, err := connect(ctx, cfg)
	if err != nil {
		fatal(err)
	}
	defer db.Close()

	schemaCols, err := fetchSchema(ctx, db, cfg.Schema, cfg.Table)
	if err != nil {
		fatal(err)
	}

	rows, err := fetchRows(ctx, db, cfg.Schema, cfg.Table, 100)
	if err != nil {
		fatal(err)
	}

	if err := os.MkdirAll(cfg.ExportDir, 0o755); err != nil {
		fatal(err)
	}

	schemaPath := filepath.Join(cfg.ExportDir, fmt.Sprintf("%s_schema.csv", strings.ToLower(cfg.Table)))
	rowsPath := filepath.Join(cfg.ExportDir, fmt.Sprintf("%s_first100.csv", strings.ToLower(cfg.Table)))
	metaPath := filepath.Join(cfg.ExportDir, fmt.Sprintf("%s_export_meta.txt", strings.ToLower(cfg.Table)))

	if err := writeSchemaCSV(schemaPath, schemaCols); err != nil {
		fatal(err)
	}
	if err := writeRowsCSV(rowsPath, rows); err != nil {
		fatal(err)
	}
	if err := os.WriteFile(metaPath, []byte(fmt.Sprintf(
		"endpoint=%s\nservice=%s\nschema=%s\ntable=%s\nschema_file=%s\nrows_file=%s\nrows_exported=%d\n",
		usedEndpoint.Host,
		usedEndpoint.Service,
		cfg.Schema,
		cfg.Table,
		schemaPath,
		rowsPath,
		len(rows),
	)), 0o644); err != nil {
		fatal(err)
	}

	fmt.Printf("conectado a %s/%s\n", usedEndpoint.Host, usedEndpoint.Service)
	fmt.Printf("esquema guardado en: %s\n", schemaPath)
	fmt.Printf("filas guardadas en: %s\n", rowsPath)
	fmt.Printf("meta guardada en: %s\n", metaPath)
}

func parseConfig() (exportConfig, error) {
	var cfg exportConfig
	var endpointsArg string

	flag.StringVar(&cfg.User, "user", envOr("ORACLE_USER", ""), "Oracle user")
	flag.StringVar(&cfg.Password, "password", envOr("ORACLE_PASSWORD", ""), "Oracle password")
	flag.StringVar(&cfg.Schema, "schema", envOr("ORACLE_SCHEMA", "DIGITALIZACION"), "Oracle schema/owner")
	flag.StringVar(&cfg.Table, "table", envOr("ORACLE_TABLE", "DIGITALIZACION"), "Oracle table")
	flag.StringVar(&cfg.ExportDir, "out", envOr("ORACLE_EXPORT_DIR", "out/oracle_export"), "Output directory")
	flag.IntVar(&cfg.Port, "port", envInt("ORACLE_PORT", 1521), "Oracle port")
	flag.DurationVar(&cfg.Timeout, "timeout", envDuration("ORACLE_TIMEOUT", 2*time.Minute), "Overall timeout")
	flag.StringVar(&endpointsArg, "endpoints", envOr("ORACLE_ENDPOINTS", "172.16.60.20:PRDSGH1,172.16.60.20:PRDSGH2,172.16.60.21:PRDSGH"), "Comma-separated host:service list")
	flag.Parse()

	if cfg.User == "" {
		return exportConfig{}, errors.New("missing Oracle user: set ORACLE_USER or pass --user")
	}
	if cfg.Password == "" {
		return exportConfig{}, errors.New("missing Oracle password: set ORACLE_PASSWORD or pass --password")
	}

	for _, item := range strings.Split(endpointsArg, ",") {
		item = strings.TrimSpace(item)
		if item == "" {
			continue
		}
		host, service, ok := strings.Cut(item, ":")
		if !ok || host == "" || service == "" {
			return exportConfig{}, fmt.Errorf("invalid endpoint %q, expected host:service", item)
		}
		cfg.Endpoints = append(cfg.Endpoints, endpoint{Host: host, Service: service})
	}

	if len(cfg.Endpoints) == 0 {
		return exportConfig{}, errors.New("no Oracle endpoints provided")
	}

	return cfg, nil
}

func connect(ctx context.Context, cfg exportConfig) (*sql.DB, endpoint, error) {
	var lastErr error
	for _, ep := range cfg.Endpoints {
		connStr := go_ora.BuildUrl(ep.Host, cfg.Port, ep.Service, cfg.User, cfg.Password, nil)
		db, err := sql.Open("oracle", connStr)
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

	return nil, endpoint{}, fmt.Errorf("could not connect to any Oracle endpoint: %w", lastErr)
}

func fetchSchema(ctx context.Context, db *sql.DB, schema, table string) ([]columnInfo, error) {
	query := `
SELECT column_id, column_name, data_type, data_length, data_precision, data_scale, nullable
FROM all_tab_columns
WHERE owner = :1 AND table_name = :2
ORDER BY column_id`

	rows, err := db.QueryContext(ctx, query, strings.ToUpper(schema), strings.ToUpper(table))
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var cols []columnInfo
	for rows.Next() {
		var c columnInfo
		if err := rows.Scan(&c.ColumnID, &c.ColumnName, &c.DataType, &c.DataLength, &c.DataPrecision, &c.DataScale, &c.Nullable); err != nil {
			return nil, err
		}
		cols = append(cols, c)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return cols, nil
}

func fetchRows(ctx context.Context, db *sql.DB, schema, table string, limit int) ([][]string, error) {
	query := fmt.Sprintf(`SELECT * FROM %s.%s WHERE ROWNUM <= %d`, oracleIdent(schema), oracleIdent(table), limit)

	rows, err := db.QueryContext(ctx, query)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	columns, err := rows.Columns()
	if err != nil {
		return nil, err
	}

	var out [][]string
	for rows.Next() {
		values := make([]any, len(columns))
		ptrs := make([]any, len(columns))
		for i := range values {
			ptrs[i] = &values[i]
		}

		if err := rows.Scan(ptrs...); err != nil {
			return nil, err
		}

		row := make([]string, len(columns))
		for i, value := range values {
			row[i] = stringify(value)
		}
		out = append(out, row)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	return out, nil
}

func writeSchemaCSV(path string, cols []columnInfo) error {
	file, err := os.Create(path)
	if err != nil {
		return err
	}
	defer file.Close()

	w := csv.NewWriter(file)
	defer w.Flush()

	if err := w.Write([]string{"column_id", "column_name", "data_type", "data_length", "data_precision", "data_scale", "nullable"}); err != nil {
		return err
	}
	for _, c := range cols {
		record := []string{
			strconv.Itoa(c.ColumnID),
			c.ColumnName,
			c.DataType,
			nullIntToString(c.DataLength),
			nullIntToString(c.DataPrecision),
			nullIntToString(c.DataScale),
			c.Nullable,
		}
		if err := w.Write(record); err != nil {
			return err
		}
	}
	return w.Error()
}

func writeRowsCSV(path string, rows [][]string) error {
	file, err := os.Create(path)
	if err != nil {
		return err
	}
	defer file.Close()

	w := csv.NewWriter(file)
	defer w.Flush()

	for _, row := range rows {
		if err := w.Write(row); err != nil {
			return err
		}
	}
	return w.Error()
}

func oracleIdent(value string) string {
	return `"` + strings.ReplaceAll(strings.ToUpper(strings.TrimSpace(value)), `"`, `""`) + `"`
}

func stringify(value any) string {
	switch v := value.(type) {
	case nil:
		return ""
	case []byte:
		return string(v)
	case time.Time:
		return v.Format(time.RFC3339Nano)
	case fmt.Stringer:
		return v.String()
	default:
		return fmt.Sprint(v)
	}
}

func nullIntToString(v sql.NullInt64) string {
	if !v.Valid {
		return ""
	}
	return strconv.FormatInt(v.Int64, 10)
}

func envOr(key, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}

func envInt(key string, fallback int) int {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		if parsed, err := strconv.Atoi(value); err == nil {
			return parsed
		}
	}
	return fallback
}

func envDuration(key string, fallback time.Duration) time.Duration {
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		return fallback
	}
	parsed, err := time.ParseDuration(value)
	if err != nil {
		return fallback
	}
	return parsed
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "error:", err)
	os.Exit(1)
}
