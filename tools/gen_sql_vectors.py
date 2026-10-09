#!/usr/bin/env python3
"""Writes SQL statements with the tables sqlglot says they read and write,
one JSON line each, for the code_facts SQL test:

    python3 tools/gen_sql_vectors.py [N] [SEED] > tests/fixtures/sql_table_vectors.jsonl
    python3 tools/gen_sql_vectors.py --files FILE.sql... > vectors.jsonl

Each line: {"sql": ..., "reads": [...], "writes": [...]} (names lower-cased,
dotted, sorted). Generated statements that sqlglot cannot parse are dropped.
"""
import json, logging, random, sys
import sqlglot
logging.getLogger("sqlglot").setLevel(logging.CRITICAL)
from sqlglot import exp

def names(table):
    parts = [p.name for p in (table.args.get("catalog"), table.args.get("db"), table.this) if p is not None and hasattr(p, "name")]
    return ".".join(parts).lower()

def analyze(sql, dialect=None):
    if dialect is None:
        dialect = "mysql" if "`" in sql else "postgres"
    try:
        trees = [t for t in sqlglot.parse(sql, read=dialect) if t is not None]
    except Exception:
        return None
    # A statement sqlglot only recognizes as a bare command has no tables.
    if any(isinstance(n, exp.Command) for t in trees for n in t.find_all(exp.Command)):
        return None
    reads, writes = set(), set()
    for tree in trees:
        # GRANT and REVOKE name tables but neither read nor write them.
        # Only statements that read or write data count: not GRANT, COMMENT ON, SET...
        if not isinstance(tree, (exp.Query, exp.Insert, exp.Update, exp.Delete, exp.Create, exp.Drop, exp.Alter, exp.Merge, exp.TruncateTable)):
            continue
        # Functions, types, indexes and the like are not tables.
        if isinstance(tree, (exp.Create, exp.Drop)) and (tree.args.get("kind") or "").upper() not in ("TABLE", "VIEW"):
            continue
        ctes = {c.alias.lower() for c in tree.find_all(exp.CTE)}
        targets = []
        for node in tree.find_all(exp.Insert, exp.Update, exp.Delete, exp.Create, exp.Drop, exp.Alter, exp.Merge, exp.TruncateTable):
            if isinstance(node, exp.TruncateTable):
                targets += [e for e in node.expressions if isinstance(e, exp.Table)]
                continue
            if isinstance(node, exp.Create) and (node.args.get("kind") or "").upper() not in ("TABLE", "VIEW"):
                continue
            if isinstance(node, exp.Drop) and (node.args.get("kind") or "").upper() not in ("TABLE", "VIEW"):
                continue
            if isinstance(node, exp.Drop):
                targets += [e for e in node.args.get("tables") or [] if isinstance(e, exp.Table)]
            this = node.this
            if isinstance(this, exp.Schema):
                this = this.this
            if isinstance(this, exp.Table):
                targets.append(this)
        # SELECT ... INTO t creates t.
        for node in tree.find_all(exp.Select):
            into = node.args.get("into")
            if into is not None and isinstance(into.this, exp.Table):
                targets.append(into.this)
        ids = {id(t) for t in targets}
        for t in targets:
            if isinstance(t.this, exp.Identifier):
                writes.add(names(t))
        for t in tree.find_all(exp.Table):
            if id(t) in ids or not isinstance(t.this, exp.Identifier):
                continue
            n = names(t)
            if n in ctes:
                continue
            reads.add(n)
    return sorted(reads), sorted(writes)

# ---- a statement generator ----
R = random.Random()
TABLES = ["orders", "customers", "sales_2024", "dim_date", "events", "users", "stg.payments", "raw.web_logs", "a", "t1", "t2", "inventory", "public.items", "x_y"]
COLS = ["id", "name", "amount", "created_at", "user_id", "status", "region", "qty"]

def tbl():
    t = R.choice(TABLES)
    q = R.random()
    if q < 0.12:
        return '"' + t.replace(".", '"."') + '"'
    if q < 0.18:
        return "`" + t.replace(".", "`.`") + "`"
    return t

def alias():
    return R.choice(["", "", " x", " AS y", " AS \"q\""])

def cols(n=None):
    return ", ".join(R.sample(COLS, n or R.randint(1, 4)))

def cond(depth=0):
    c = R.choice(["id > 3", "status = 'ok'", "amount BETWEEN 1 AND 5", "name LIKE '%from t%'", "x IS NOT NULL"])
    if depth < 2 and R.random() < 0.3:
        c = f"id IN ({select(depth + 1)})"
    if depth < 2 and R.random() < 0.15:
        c = f"EXISTS ({select(depth + 1)})"
    if R.random() < 0.3:
        c += f" AND {R.choice(['a = 1', 'b <> 2'])}"
    return c

def source(depth):
    r = R.random()
    if r < 0.12 and depth < 2:
        return f"({select(depth + 1)}) AS sub{depth}"
    if r < 0.17:
        return "generate_series(1, 10) AS g"
    if r < 0.2:
        return "(VALUES (1, 2)) AS v(a, b)"
    return tbl() + alias()

def select(depth=0):
    parts = []
    if R.random() < 0.2 and depth == 0:
        names_ = [R.choice(["recent", "base", "tmp", "t1"]) for _ in range(R.randint(1, 2))]
        names_ = list(dict.fromkeys(names_))
        ctes = ", ".join(f"{n} AS ({select(depth + 1)})" for n in names_)
        parts.append(f"WITH {ctes}")
        pool = names_
    else:
        pool = []
    sel = R.choice(["*", cols(), "COUNT(*) AS n", "EXTRACT(year FROM created_at) AS y", "TRIM(BOTH ' ' FROM name) AS nm", "SUBSTRING(name FROM 1 FOR 3) AS s", "SUM(amount)"])
    first = R.choice(pool) if pool and R.random() < 0.8 else source(depth)
    q = f"SELECT {'DISTINCT ' if R.random() < 0.1 else ''}{sel} FROM {first}{alias() if first in pool else ''}"
    for _ in range(R.choice([0, 0, 1, 2])):
        kind = R.choice(["JOIN", "LEFT JOIN", "INNER JOIN", "RIGHT JOIN", "FULL OUTER JOIN", "CROSS JOIN"])
        q += f" {kind} {source(depth)}"
        if kind != "CROSS JOIN":
            q += R.choice([" ON a.id = b.id", " USING (id)"])
    if R.random() < 0.15:
        q += f", {tbl()}{alias()}"
    if R.random() < 0.6:
        q += f" WHERE {cond(depth)}"
    if R.random() < 0.2:
        q += f" GROUP BY {R.choice(COLS)}"
    if R.random() < 0.2:
        q += f" ORDER BY {R.choice(COLS)} DESC LIMIT 10"
    if R.random() < 0.15 and depth < 2:
        q += f" {R.choice(['UNION', 'UNION ALL', 'INTERSECT', 'EXCEPT'])} {select(depth + 1)}"
    parts.append(q)
    return " ".join(parts)

def statement():
    k = R.random()
    if k < 0.40:
        s = select()
    elif k < 0.50:
        s = f"INSERT INTO {tbl()} ({cols(2)}) {select(1)}"
    elif k < 0.55:
        s = f"INSERT INTO {tbl()} VALUES (1, 'a from b', 3)"
    elif k < 0.62:
        s = f"UPDATE {tbl()}{alias()} SET status = 'x', qty = qty + 1 WHERE {cond()}"
    elif k < 0.66:
        s = f"UPDATE {tbl()} SET amount = s.amount FROM {tbl()} s WHERE s.id = id"
    elif k < 0.72:
        s = f"DELETE FROM {tbl()} WHERE {cond()}"
    elif k < 0.75:
        s = f"DELETE FROM {tbl()} USING {tbl()} u WHERE u.id = id"
    elif k < 0.80:
        s = f"CREATE TABLE {R.choice(['', 'IF NOT EXISTS '])}{tbl()} AS {select(1)}"
    elif k < 0.84:
        s = f"CREATE TABLE {tbl()} (id INT PRIMARY KEY, name VARCHAR(20), amount DECIMAL(10, 2))"
    elif k < 0.89:
        s = f"CREATE {R.choice(['', 'OR REPLACE '])}VIEW {tbl()} AS {select(1)}"
    elif k < 0.92:
        s = f"DROP TABLE {R.choice(['', 'IF EXISTS '])}{tbl()}"
    elif k < 0.94:
        s = f"ALTER TABLE {tbl()} ADD COLUMN note TEXT"
    elif k < 0.96:
        s = f"TRUNCATE TABLE {tbl()}"
    elif k < 0.98:
        s = f"MERGE INTO {tbl()} t USING {tbl()} s ON t.id = s.id WHEN MATCHED THEN UPDATE SET amount = s.amount WHEN NOT MATCHED THEN INSERT (id) VALUES (s.id)"
    else:
        s = f"SELECT 1 /* from hidden_a */ FROM {tbl()} -- join hidden_b"
    if R.random() < 0.1:
        s = s.replace(" ", "\n  ", 3)
    if R.random() < 0.1:
        s = f"-- from not_a_table\n{s}"
    return s

def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--files":
        from sqlglot.tokens import TokenType
        for f in sys.argv[2:]:
            sql = open(f, encoding="utf-8", errors="replace").read()
            try:
                toks = sqlglot.Dialect.get_or_raise("postgres").tokenize(sql)
            except Exception:
                continue
            start = 0
            for tok in toks:
                if tok.token_type == TokenType.SEMICOLON:
                    stmt = sql[start:tok.end + 1].strip()
                    start = tok.end + 1
                    r = analyze(stmt)
                    if r is not None and stmt:
                        print(json.dumps({"sql": stmt, "reads": r[0], "writes": r[1]}))
        return
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 1500
    R.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 7)
    seen = set()
    while len(seen) < n:
        s = statement()
        if s in seen:
            continue
        r = analyze(s)
        if r is None:
            continue
        seen.add(s)
        print(json.dumps({"sql": s, "reads": r[0], "writes": r[1]}))

main()
