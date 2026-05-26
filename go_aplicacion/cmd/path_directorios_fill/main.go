package main

import (
	"context"
	"database/sql"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/sijms/go-ora/v2"
	_ "modernc.org/sqlite"
)

type endpoint struct {
	Host    string
	Service string
}

type config struct {
	OracleUser    string
	OraclePass    string
	OracleSchema  string
	OracleTable   string
	OracleField   string
	OracleValue   string
	SQLiteDSN     string
	SQLiteTable   string
	SQLiteNameCol string
	SQLitePathCol string
	OutputFile    string
	Endpoints     []endpoint
	Port          int
	Timeout       time.Duration
}

func main() {
	cfg, err := parseConfig()
	if err != nil {
		fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), cfg.Timeout)
	defer cancel()

	paths, meta, err := buildPaths(ctx, cfg)
	if err != nil {
		fatal(err)
	}

	if err := os.MkdirAll(filepath.Dir(cfg.OutputFile), 0o755); err != nil {
		fatal(err)
	}

	content := strings.Join(paths, "\n")
	if len(paths) > 0 {
		content += "\n"
	}
	if err := os.WriteFile(cfg.OutputFile, []byte(content), 0o644); err != nil {
		fatal(err)
	}

	metaPath := strings.TrimSuffix(cfg.OutputFile, filepath.Ext(cfg.OutputFile)) + ".meta.txt"
	if err := os.WriteFile(metaPath, []byte(meta), 0o644); err != nil {
		fatal(err)
	}

	fmt.Printf("filas Oracle encontradas: %d\n", metaValueCount(meta, "oracle_rows="))
	fmt.Printf("rutas SQLite generadas: %d\n", len(paths))
	fmt.Printf("archivo escrito en: %s\n", cfg.OutputFile)
	fmt.Printf("meta escrita en: %s\n", metaPath)
}

func parseConfig() (config, error) {
	var cfg config
	var endpointsArg string

	flag.StringVar(&cfg.OracleUser, "oracle-user", envOr("ORACLE_USER", ""), "Oracle user")
	flag.StringVar(&cfg.OraclePass, "oracle-password", envOr("ORACLE_PASSWORD", ""), "Oracle password")
	flag.StringVar(&cfg.OracleSchema, "oracle-schema", envOr("ORACLE_SCHEMA", "DIGITALIZACION"), "Oracle schema/owner")
	flag.StringVar(&cfg.OracleTable, "oracle-table", envOr("ORACLE_TABLE", "DIGITALIZACION"), "Oracle table")
	flag.StringVar(&cfg.OracleField, "field", envOr("ORACLE_FILTER_FIELD", "DIG_ID_GENERACION"), "Oracle filter field")
	flag.StringVar(&cfg.OracleValue, "value", envOr("ORACLE_FILTER_VALUE", ""), "Oracle filter value")
	flag.StringVar(&cfg.SQLiteDSN, "sqlite-dsn", envOr("SQLITE_DSN", "file:/data_nuevo/repo_grande/data/folders.sqlite"), "SQLite DSN")
	flag.StringVar(&cfg.SQLiteTable, "sqlite-table", envOr("SQLITE_TABLE", "folders"), "SQLite table")
	flag.StringVar(&cfg.SQLiteNameCol, "sqlite-name-col", envOr("SQLITE_NAME_COL", "name"), "SQLite name column")
	flag.StringVar(&cfg.SQLitePathCol, "sqlite-path-col", envOr("SQLITE_PATH_COL", "path"), "SQLite path column")
	flag.StringVar(&cfg.OutputFile, "out", envOr("PATH_DIRECTORIOS_OUT", "../fuentes_txt/PATH_DIRECTORIOS.txt"), "Output txt file")
	flag.IntVar(&cfg.Port, "port", envInt("ORACLE_PORT", 1521), "Oracle port")
	flag.DurationVar(&cfg.Timeout, "timeout", envDuration("ORACLE_TIMEOUT", 2*time.Minute), "Overall timeout")
	flag.StringVar(&endpointsArg, "endpoints", envOr("ORACLE_ENDPOINTS", "172.16.60.20:PRDSGH1,172.16.60.20:PRDSGH2,172.16.60.21:PRDSGH"), "Comma-separated host:service list")
	flag.Parse()

	if cfg.OracleUser == "" {
		return config{}, errors.New("missing Oracle user: set ORACLE_USER")
	}
	if cfg.OraclePass == "" {
		return config{}, errors.New("missing Oracle password: set ORACLE_PASSWORD")
	}
	if cfg.OracleValue == "" {
		return config{}, errors.New("missing filter value: set ORACLE_FILTER_VALUE or pass --value")
	}

	for _, item := range strings.Split(endpointsArg, ",") {
		item = strings.TrimSpace(item)
		if item == "" {
			continue
		}
		host, service, ok := strings.Cut(item, ":")
		if !ok || host == "" || service == "" {
			return config{}, fmt.Errorf("invalid endpoint %q, expected host:service", item)
		}
		cfg.Endpoints = append(cfg.Endpoints, endpoint{Host: host, Service: service})
	}
	if len(cfg.Endpoints) == 0 {
		return config{}, errors.New("no Oracle endpoints provided")
	}

	return cfg, nil
}

func buildPaths(ctx context.Context, cfg config) ([]string, string, error) {
	oracleDB, usedEndpoint, err := connectOracle(ctx, cfg)
	if err != nil {
		return nil, "", err
	}
	defer oracleDB.Close()

	sqliteDB, err := sql.Open("sqlite", cfg.SQLiteDSN)
	if err != nil {
		return nil, "", fmt.Errorf("open sqlite: %w", err)
	}
	defer sqliteDB.Close()

	if err := sqliteDB.PingContext(ctx); err != nil {
		return nil, "", fmt.Errorf("ping sqlite: %w", err)
	}

	oracleRows, err := fetchOracleValues(ctx, oracleDB, cfg.OracleSchema, cfg.OracleTable, cfg.OracleField, cfg.OracleValue)
	if err != nil {
		return nil, "", err
	}

	if len(oracleRows) == 0 {
		return nil, "", fmt.Errorf("no Oracle rows matched %s=%s", cfg.OracleField, cfg.OracleValue)
	}

	pathsSet := make(map[string]struct{})
	var pathList []string
	for _, tramite := range oracleRows {
		sqlitePaths, err := fetchSQLitePaths(ctx, sqliteDB, cfg.SQLiteTable, cfg.SQLiteNameCol, cfg.SQLitePathCol, tramite)
		if err != nil {
			return nil, "", err
		}
		for _, p := range sqlitePaths {
			if _, ok := pathsSet[p]; ok {
				continue
			}
			pathsSet[p] = struct{}{}
			pathList = append(pathList, p)
		}
	}

	sort.Strings(pathList)

	meta := strings.Join([]string{
		fmt.Sprintf("endpoint=%s", usedEndpoint.Host),
		fmt.Sprintf("service=%s", usedEndpoint.Service),
		fmt.Sprintf("oracle_schema=%s", cfg.OracleSchema),
		fmt.Sprintf("oracle_table=%s", cfg.OracleTable),
		fmt.Sprintf("oracle_field=%s", cfg.OracleField),
		fmt.Sprintf("oracle_value=%s", cfg.OracleValue),
		fmt.Sprintf("oracle_rows=%d", len(oracleRows)),
		fmt.Sprintf("sqlite_table=%s", cfg.SQLiteTable),
		fmt.Sprintf("sqlite_name_col=%s", cfg.SQLiteNameCol),
		fmt.Sprintf("sqlite_path_col=%s", cfg.SQLitePathCol),
		fmt.Sprintf("sqlite_paths=%d", len(pathList)),
		fmt.Sprintf("output_file=%s", cfg.OutputFile),
	}, "\n") + "\n"

	return pathList, meta, nil
}

func connectOracle(ctx context.Context, cfg config) (*sql.DB, endpoint, error) {
	var lastErr error
	for _, ep := range cfg.Endpoints {
		connStr := go_ora.BuildUrl(ep.Host, cfg.Port, ep.Service, cfg.OracleUser, cfg.OraclePass, nil)
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

func fetchOracleValues(ctx context.Context, db *sql.DB, schema, table, field, value string) ([]string, error) {
	if err := validateIdent(field); err != nil {
		return nil, fmt.Errorf("invalid Oracle field: %w", err)
	}
	if err := validateIdent(schema); err != nil {
		return nil, fmt.Errorf("invalid Oracle schema: %w", err)
	}
	if err := validateIdent(table); err != nil {
		return nil, fmt.Errorf("invalid Oracle table: %w", err)
	}

	query := fmt.Sprintf(
		`SELECT DISTINCT DIG_TRAMITE FROM %s.%s WHERE %s = :1 AND DIG_PLANILLADO = 'S'`,
		oracleIdent(schema),
		oracleIdent(table),
		oracleIdent(field),
	)
	rows, err := db.QueryContext(ctx, query, value)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var out []string
	for rows.Next() {
		var tramite sql.NullString
		if err := rows.Scan(&tramite); err != nil {
			return nil, err
		}
		if tramite.Valid {
			out = append(out, strings.TrimSpace(tramite.String))
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return out, nil
}

func fetchSQLitePaths(ctx context.Context, db *sql.DB, table, nameCol, pathCol, tramite string) ([]string, error) {
	if err := validateIdent(table); err != nil {
		return nil, fmt.Errorf("invalid SQLite table: %w", err)
	}
	if err := validateIdent(nameCol); err != nil {
		return nil, fmt.Errorf("invalid SQLite name column: %w", err)
	}
	if err := validateIdent(pathCol); err != nil {
		return nil, fmt.Errorf("invalid SQLite path column: %w", err)
	}

	query := fmt.Sprintf(`SELECT %s FROM %s WHERE %s = ? ORDER BY %s`, sqliteIdent(pathCol), sqliteIdent(table), sqliteIdent(nameCol), sqliteIdent(pathCol))
	rows, err := db.QueryContext(ctx, query, tramite)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var out []string
	for rows.Next() {
		var path sql.NullString
		if err := rows.Scan(&path); err != nil {
			return nil, err
		}
		if path.Valid && strings.TrimSpace(path.String) != "" {
			out = append(out, path.String)
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return out, nil
}

func validateIdent(value string) error {
	value = strings.TrimSpace(value)
	if value == "" {
		return errors.New("empty identifier")
	}
	for _, r := range value {
		if r == '_' || r == '$' || r == '#' || r == '.' {
			continue
		}
		if r >= 'A' && r <= 'Z' {
			continue
		}
		if r >= 'a' && r <= 'z' {
			continue
		}
		if r >= '0' && r <= '9' {
			continue
		}
		return fmt.Errorf("invalid character %q", r)
	}
	return nil
}

func oracleIdent(value string) string {
	return strings.ToUpper(strings.TrimSpace(value))
}

func sqliteIdent(value string) string {
	return `"` + strings.ReplaceAll(strings.TrimSpace(value), `"`, `""`) + `"`
}

func metaValueCount(meta, keyPrefix string) int {
	for _, line := range strings.Split(meta, "\n") {
		if strings.HasPrefix(line, keyPrefix) {
			value := strings.TrimPrefix(line, keyPrefix)
			n, _ := strconv.Atoi(strings.TrimSpace(value))
			return n
		}
	}
	return 0
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
