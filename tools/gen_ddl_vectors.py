#!/usr/bin/env python3
"""Random schemas, loaded into real PostgreSQL and MySQL servers, dumped, and read back.

    python3 tools/gen_ddl_vectors.py OUT.jsonl [--seeds 40] [--pg-port 54329] [--my-port 33069]

Each line is {"dialect", "sql", "tables"}: `sql` is what pg_dump
(--schema-only) or mysqldump (--no-data) writes for the schema, and `tables`
is what the server's own catalog says: for each table its columns in order,
primary key, and foreign keys as (columns, referenced table, referenced
columns). `cargo test` checks the schema reader in src/lib.rs against these.

Needs psql/pg_dump 17 and mysql/mysqldump on PATH (Homebrew:
/opt/homebrew/opt/postgresql@17/bin, /opt/homebrew/opt/mysql/bin) and the
two servers running (see CLAUDE.md, "real-engine verification").
"""
import argparse
import json
import random
import subprocess
import sys

PG_TYPES = ["integer", "bigint", "smallint", "text", "varchar(40)", "character varying(255)",
            "numeric(10,2)", "boolean", "date", "timestamp", "timestamp with time zone",
            "uuid", "jsonb", "integer[]", "bytea", "double precision", "char(3)", "real",
            "time", "interval"]
MY_TYPES = ["int", "bigint", "smallint", "tinyint(1)", "mediumint", "varchar(40)", "text",
            "decimal(10,2)", "datetime", "timestamp NULL", "json", "double", "date",
            "char(3)", "blob", "enum('a','b')", "int unsigned", "bigint unsigned"]
WEIRD = ["plain", "Mixed Case", "with space", 'quo"te', "tab`le", "ünï", "select", "order", "a-b", "x.y"]
INT_PG = ["integer", "bigint", "smallint"]
INT_MY = ["int", "bigint", "smallint", "mediumint"]


def q_pg(name):
    return '"' + name.replace('"', '""') + '"'


def q_my(name):
    return "`" + name.replace("`", "``") + "`"


def make_schema(rng, dialect):
    q = q_pg if dialect == "postgres" else q_my
    ints = INT_PG if dialect == "postgres" else INT_MY
    types = PG_TYPES if dialect == "postgres" else MY_TYPES
    tables = []
    used = set()
    for t in range(rng.randint(2, 6)):
        name = f"t{t}_" + rng.choice(WEIRD)
        if dialect == "mysql":
            name = name.replace("x.y", "x_y")
        used.add(name)
        pk_n = rng.choice([1, 1, 1, 2])
        cols = []
        pk = []
        for k in range(pk_n):
            ty = rng.choice(ints)
            cn = f"k{k}_" + rng.choice(WEIRD[:5])
            cols.append((cn, ty, True))
            pk.append(cn)
        for k in range(rng.randint(1, 6)):
            cn = f"c{k}_" + rng.choice(WEIRD)
            if dialect == "mysql":
                cn = cn.replace("x.y", "x_y")
            if any(c[0] == cn for c in cols):
                continue
            ty = rng.choice(types)
            cols.append((cn, ty, rng.random() < 0.3))
        fks = []
        # Foreign keys to earlier tables' primary keys (and sometimes itself).
        for prev in tables + ([None] if rng.random() < 0.2 else []):
            if rng.random() < 0.6:
                continue
            target = prev or {"name": name, "cols": cols, "pk": pk}
            tcols = [(c[0], c[1]) for c in target["cols"] if c[0] in target["pk"]]
            local = []
            for i, (tn, tty) in enumerate(tcols):
                ln = f"ref{len(fks)}_{i}_" + rng.choice(WEIRD[:4])
                cols.append((ln, tty, False))
                local.append(ln)
            fks.append({"cols": local, "table": target["name"], "target": [c[0] for c in tcols]})
        tables.append({"name": name, "cols": cols, "pk": pk, "fks": fks})
    return tables


def ddl(tables, dialect, schema_prefix=""):
    q = q_pg if dialect == "postgres" else q_my
    out = []
    for t in tables:
        lines = []
        for cn, ty, nullable in t["cols"]:
            nn = "" if nullable else " NOT NULL"
            if dialect == "mysql" and "timestamp NULL" in ty:
                nn = ""
            lines.append(f"{q(cn)} {ty}{nn}")
        lines.append("PRIMARY KEY (" + ", ".join(q(c) for c in t["pk"]) + ")")
        out.append(f"CREATE TABLE {schema_prefix}{q(t['name'])} (\n  " + ",\n  ".join(lines) + "\n)" +
                   (" ENGINE=InnoDB" if dialect == "mysql" else "") + ";")
    # Foreign keys last, so order does not matter.
    for t in tables:
        for i, fk in enumerate(t["fks"]):
            out.append(
                f"ALTER TABLE {schema_prefix}{q(t['name'])} ADD CONSTRAINT {q('fk_' + str(i) + '_' + t['name'])[:60] if dialect == 'postgres' else 'fk_' + str(i) + '_' + str(abs(hash(t['name'])) % 9999)} "
                f"FOREIGN KEY ({', '.join(q(c) for c in fk['cols'])}) REFERENCES {schema_prefix}{q(fk['table'])} "
                f"({', '.join(q(c) for c in fk['target'])});"
            )
    return out


def psql(port, db, sql, tuples=False):
    cmd = ["psql", "-h", "/tmp/sniffdb", "-p", str(port), "-U", "postgres", "-d", db, "-v", "ON_ERROR_STOP=1", "-X", "-q"]
    if tuples:
        cmd += ["-At"]
    r = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(r.stderr)
    return r.stdout


PG_CATALOG = """
select json_agg(json_build_object(
  'name', c.relname,
  'columns', (select json_agg(a.attname order by a.attnum) from pg_attribute a where a.attrelid=c.oid and a.attnum>0 and not a.attisdropped),
  'pk', (select (select json_agg(a.attname order by k.ord) from unnest(p.conkey) with ordinality k(attnum, ord) join pg_attribute a on a.attrelid=c.oid and a.attnum=k.attnum) from pg_constraint p where p.conrelid=c.oid and p.contype='p'),
  'fks', coalesce((select json_agg(json_build_object(
      'cols', (select json_agg(a.attname order by k.ord) from unnest(f.conkey) with ordinality k(attnum, ord) join pg_attribute a on a.attrelid=f.conrelid and a.attnum=k.attnum),
      'table', rc.relname,
      'target', (select json_agg(a.attname order by k.ord) from unnest(f.confkey) with ordinality k(attnum, ord) join pg_attribute a on a.attrelid=f.confrelid and a.attnum=k.attnum))
      order by f.conname) from pg_constraint f join pg_class rc on rc.oid=f.confrelid where f.conrelid=c.oid and f.contype='f'), '[]'::json)
) order by c.relname)
from pg_class c join pg_namespace n on n.oid=c.relnamespace where c.relkind='r' and n.nspname='public';
"""


PG_TRAPS = r"""
CREATE FUNCTION public.f_trap() RETURNS void LANGUAGE plpgsql AS $$ BEGIN CREATE TABLE trap_inside (id int); END $$;
CREATE FUNCTION public.f_tag() RETURNS text LANGUAGE plpgsql AS $body$ BEGIN PERFORM 'x;'; CREATE TABLE trap_two (id int); RETURN 'x'; END $body$;
COMMENT ON TABLE public.@FIRST@ IS 'has ; semicolon, -- dashes, /* and \ backslash';
CREATE TABLE public.data_trap (id integer PRIMARY KEY, txt text DEFAULT 'a;b' NOT NULL, "weird;col" text);
INSERT INTO public.data_trap VALUES (1, 'semi;colon', 'x'), (2, 'quote''s', 'y'), (3, E'back\\slash', 'z'),
  (4, '-- not a comment', ';'), (5, '/* not a comment */', 'CREATE TABLE fake (id int);');
"""

MY_TRAPS = r"""
CREATE TABLE data_trap (id int PRIMARY KEY, txt varchar(100) DEFAULT 'a;b' NOT NULL, `weird;col` text);
INSERT INTO data_trap VALUES (1, 'semi;colon', 'x'), (2, 'quote\'s', 'y'), (3, 'back\\slash', 'z'),
  (4, '-- not a comment', ';'), (5, '/* not a comment */', 'CREATE TABLE fake (id int);');
DELIMITER ;;
CREATE PROCEDURE p_trap() BEGIN CREATE TABLE trap_inside (id int); SELECT 'x;'; END;;
CREATE TRIGGER trg_trap BEFORE INSERT ON data_trap FOR EACH ROW BEGIN SET NEW.txt = CONCAT(NEW.txt, ';'); END;;
DELIMITER ;
"""


def pg_vector(rng, port, seed):
    tables = make_schema(rng, "postgres")
    db = f"ddl{seed}"
    psql(port, "postgres", f'DROP DATABASE IF EXISTS "{db}"; CREATE DATABASE "{db}";')
    psql(port, db, "\n".join(ddl(tables, "postgres")))
    # Things in a dump that look like DDL but are not: a function body, a
    # comment, defaults and data holding semicolons, quotes and fake statements.
    psql(port, db, PG_TRAPS.replace("@FIRST@", q_pg(tables[0]["name"])))
    args = [] if seed % 2 else ["--schema-only"]
    sql = subprocess.run(["pg_dump", "-h", "/tmp/sniffdb", "-p", str(port), "-U", "postgres", *args, db],
                         capture_output=True, text=True, check=True).stdout
    cat = json.loads(psql(port, db, PG_CATALOG, tuples=True))
    psql(port, "postgres", f'DROP DATABASE "{db}";')
    return {"dialect": "postgres", "sql": sql, "tables": cat}


def my(port, sql, db=None):
    cmd = ["mysql", "--protocol=TCP", "-h127.0.0.1", f"-P{port}", "-uroot", "-N", "-B"]
    if db:
        cmd.append(db)
    r = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(r.stderr)
    return r.stdout


def mysql_cli(port, sql, db):
    """Runs a script through the mysql client (it understands DELIMITER)."""
    r = subprocess.run(["mysql", "--protocol=TCP", "-h127.0.0.1", f"-P{port}", "-uroot", db],
                       input=sql, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(r.stderr)


def my_vector(rng, port, seed):
    tables = make_schema(rng, "mysql")
    db = f"ddl{seed}"
    my(port, f"DROP DATABASE IF EXISTS `{db}`; CREATE DATABASE `{db}`;")
    my(port, "SET FOREIGN_KEY_CHECKS=0;\n" + "\n".join(ddl(tables, "mysql")), db)
    mysql_cli(port, MY_TRAPS, db)
    args = ["--routines", "--triggers"] + ([] if seed % 2 else ["--no-data"])
    sql = subprocess.run(["mysqldump", "--protocol=TCP", "-h127.0.0.1", f"-P{port}", "-uroot", *args, db],
                         capture_output=True, text=True, check=True).stdout
    cols = my(port, "select table_name, column_name from information_schema.columns "
                    f"where table_schema='{db}' order by table_name, ordinal_position")
    keys = my(port, "select k.table_name, k.constraint_name, k.column_name, ifnull(k.referenced_table_name,''), "
                    "ifnull(k.referenced_column_name,'') from information_schema.key_column_usage k "
                    f"where k.table_schema='{db}' order by k.table_name, k.constraint_name, k.ordinal_position")
    my(port, f"DROP DATABASE `{db}`")
    out = {}
    for line in cols.splitlines():
        t, c = line.split("\t")
        out.setdefault(t, {"name": t, "columns": [], "pk": None, "fks": []})["columns"].append(c)
    fk = {}
    for line in keys.splitlines():
        t, cn, c, rt, rc = line.split("\t")
        if cn == "PRIMARY":
            out[t]["pk"] = (out[t]["pk"] or []) + [c]
        elif rt:
            e = fk.setdefault((t, cn), {"cols": [], "table": rt, "target": []})
            e["cols"].append(c)
            e["target"].append(rc)
    for (t, _), e in sorted(fk.items()):
        out[t]["fks"].append(e)
    return {"dialect": "mysql", "sql": sql, "tables": sorted(out.values(), key=lambda t: t["name"])}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--seeds", type=int, default=40)
    ap.add_argument("--pg-port", type=int, default=54329)
    ap.add_argument("--my-port", type=int, default=33069)
    args = ap.parse_args()
    with open(args.out, "w") as f:
        for seed in range(args.seeds):
            for make, port in ((pg_vector, args.pg_port), (my_vector, args.my_port)):
                try:
                    v = make(random.Random(seed), port, seed)
                except RuntimeError as e:
                    print("skip", make.__name__, seed, str(e).strip()[:100], file=sys.stderr)
                    continue
                f.write(json.dumps(v, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
