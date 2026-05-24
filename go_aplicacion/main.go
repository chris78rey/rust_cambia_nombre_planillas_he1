package main

import (
	"context"
	"database/sql"
	"encoding/csv"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "github.com/sijms/go-ora/v2"
	_ "modernc.org/sqlite"
)

const (
	defaultOracleQuery = "SELECT table_name FROM user_tables ORDER BY table_name"
	defaultSQLiteQuery = "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
	defaultTimeout     = 30 * time.Second
)

type config struct {
	oracleDSN   string
	sqliteDSN   string
	oracleQuery string
	sqliteQuery string
	sqliteExport string
	jsonOutput  bool
	timeout     time.Duration
}

type queryResult struct {
	Name     string     `json:"name"`
	Query    string     `json:"query"`
	Columns  []string   `json:"columns"`
	Rows     [][]string `json:"rows"`
	RowCount int        `json:"row_count"`
}

type output struct {
	Oracle queryResult `json:"oracle"`
	SQLite queryResult `json:"sqlite"`
}

func main() {
	cfg, err := parseConfig()
	if err != nil {
		fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), cfg.timeout)
	defer cancel()

	result, err := run(ctx, cfg)
	if err != nil {
		fatal(err)
	}

	if cfg.jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		if err := enc.Encode(result); err != nil {
			fatal(err)
		}
		return
	}

	printText(os.Stdout, result)
}

func parseConfig() (config, error) {
	var cfg config

	flag.StringVar(&cfg.oracleDSN, "oracle-dsn", envOr("ORACLE_DSN", ""), "Oracle DSN, for example oracle://user:pass@host:1521/service")
	flag.StringVar(&cfg.sqliteDSN, "sqlite-dsn", envOr("SQLITE_DSN", "file:local.db"), "SQLite DSN, for example file:local.db or /path/to/db.sqlite")
	flag.StringVar(&cfg.oracleQuery, "oracle-query", envOr("ORACLE_QUERY", defaultOracleQuery), "SQL query to run against Oracle")
	flag.StringVar(&cfg.sqliteQuery, "sqlite-query", envOr("SQLITE_QUERY", defaultSQLiteQuery), "SQL query to run against SQLite")
	flag.StringVar(&cfg.sqliteExport, "sqlite-export", envOr("SQLITE_EXPORT", ""), "Optional file path to export the SQLite query result as CSV")
	flag.BoolVar(&cfg.jsonOutput, "json", envBool("JSON_OUTPUT", false), "Print machine-readable JSON output")
	flag.DurationVar(&cfg.timeout, "timeout", envDuration("TIMEOUT", defaultTimeout), "Overall timeout, for example 30s or 2m")
	flag.Parse()

	if strings.TrimSpace(cfg.sqliteDSN) == "" {
		return config{}, errors.New("missing SQLite DSN: set SQLITE_DSN or pass --sqlite-dsn")
	}

	return cfg, nil
}

func run(ctx context.Context, cfg config) (output, error) {
	sqliteDB, err := sql.Open("sqlite", cfg.sqliteDSN)
	if err != nil {
		return output{}, fmt.Errorf("open sqlite: %w", err)
	}
	defer sqliteDB.Close()

	if err := sqliteDB.PingContext(ctx); err != nil {
		return output{}, fmt.Errorf("ping sqlite: %w", err)
	}

	var oracleResult queryResult
	if strings.TrimSpace(cfg.oracleDSN) != "" {
		oracleDB, err := sql.Open("oracle", cfg.oracleDSN)
		if err != nil {
			return output{}, fmt.Errorf("open oracle: %w", err)
		}
		defer oracleDB.Close()

		if err := oracleDB.PingContext(ctx); err != nil {
			return output{}, fmt.Errorf("ping oracle: %w", err)
		}

		oracleResult, err = runQuery(ctx, oracleDB, "oracle", cfg.oracleQuery)
		if err != nil {
			return output{}, err
		}
	}

	sqliteResult, err := runQuery(ctx, sqliteDB, "sqlite", cfg.sqliteQuery)
	if err != nil {
		return output{}, err
	}

	if strings.TrimSpace(cfg.sqliteExport) != "" {
		if err := writeCSV(cfg.sqliteExport, sqliteResult); err != nil {
			return output{}, err
		}
	}

	return output{
		Oracle: oracleResult,
		SQLite: sqliteResult,
	}, nil
}

func runQuery(ctx context.Context, db *sql.DB, name, query string) (queryResult, error) {
	rows, err := db.QueryContext(ctx, query)
	if err != nil {
		return queryResult{}, fmt.Errorf("%s query failed: %w", name, err)
	}
	defer rows.Close()

	columns, err := rows.Columns()
	if err != nil {
		return queryResult{}, fmt.Errorf("%s columns: %w", name, err)
	}

	result := queryResult{
		Name:    name,
		Query:   query,
		Columns: columns,
	}

	for rows.Next() {
		values := make([]any, len(columns))
		ptrs := make([]any, len(columns))
		for i := range values {
			ptrs[i] = &values[i]
		}

		if err := rows.Scan(ptrs...); err != nil {
			return queryResult{}, fmt.Errorf("%s scan: %w", name, err)
		}

		row := make([]string, len(columns))
		for i, value := range values {
			row[i] = stringify(value)
		}
		result.Rows = append(result.Rows, row)
	}

	if err := rows.Err(); err != nil {
		return queryResult{}, fmt.Errorf("%s rows: %w", name, err)
	}

	result.RowCount = len(result.Rows)
	return result, nil
}

func printText(w io.Writer, result output) {
	printSection := func(title string, r queryResult) {
		if len(r.Columns) == 0 && r.RowCount == 0 && r.Query == "" {
			fmt.Fprintf(w, "%s\n", strings.ToUpper(title))
			fmt.Fprintln(w, "omitido")
			fmt.Fprintln(w)
			return
		}
		fmt.Fprintf(w, "%s\n", strings.ToUpper(title))
		fmt.Fprintf(w, "query: %s\n", r.Query)
		fmt.Fprintf(w, "columns: %s\n", strings.Join(r.Columns, ", "))
		fmt.Fprintf(w, "rows: %d\n", r.RowCount)
		if r.RowCount == 0 {
			fmt.Fprintln(w, "sin filas")
			fmt.Fprintln(w)
			return
		}
		for i, row := range r.Rows {
			pairs := make([]string, 0, len(row))
			for colIndex, colName := range r.Columns {
				pairs = append(pairs, fmt.Sprintf("%s=%s", colName, row[colIndex]))
			}
			fmt.Fprintf(w, "%d) %s\n", i+1, strings.Join(pairs, " | "))
		}
		fmt.Fprintln(w)
	}

	printSection("oracle", result.Oracle)
	printSection("sqlite", result.SQLite)
}

func writeCSV(path string, result queryResult) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create export dir: %w", err)
	}

	file, err := os.Create(path)
	if err != nil {
		return fmt.Errorf("create export file: %w", err)
	}
	defer file.Close()

	writer := csv.NewWriter(file)
	if len(result.Columns) > 0 {
		if err := writer.Write(result.Columns); err != nil {
			return fmt.Errorf("write csv header: %w", err)
		}
	}
	for _, row := range result.Rows {
		if err := writer.Write(row); err != nil {
			return fmt.Errorf("write csv row: %w", err)
		}
	}
	writer.Flush()
	if err := writer.Error(); err != nil {
		return fmt.Errorf("flush csv: %w", err)
	}

	return nil
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
	case bool:
		if v {
			return "true"
		}
		return "false"
	default:
		return fmt.Sprint(v)
	}
}

func envOr(key, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}

func envBool(key string, fallback bool) bool {
	value := strings.TrimSpace(strings.ToLower(os.Getenv(key)))
	switch value {
	case "1", "true", "yes", "y", "on":
		return true
	case "0", "false", "no", "n", "off":
		return false
	default:
		return fallback
	}
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
