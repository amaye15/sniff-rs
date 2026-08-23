#!/usr/bin/env python3
"""Regenerates benches/fixtures/comparison_data.* for format_comparison.rs.

Writes the exact same 10,000-row DataFrame to five formats (CSV, JSON,
Parquet, SQLite, Excel) so format_comparison.rs's benchmark is genuinely
apples-to-apples - same values, not just the same row count. Needs
pandas, pyarrow (for Parquet), and openpyxl (for Excel):
    pip3 install pandas pyarrow openpyxl

Only needs re-running if the dataset shape/size should change - the
generated files are committed, not built fresh by `cargo bench`.
"""

import sqlite3
import uuid as uuidlib

import numpy as np
import pandas as pd

np.random.seed(42)
n = 10_000

df = pd.DataFrame(
    {
        "id": np.arange(n),
        "name": [f"user_{i}" for i in range(n)],
        "signup_date": pd.date_range("2020-01-01", periods=n, freq="h").strftime("%Y-%m-%d"),
        "amount": np.round(np.random.uniform(0, 10000, n), 2),
        "active": np.random.choice([True, False], n),
        "user_uuid": [str(uuidlib.UUID(int=i)) for i in range(n)],
        "score": np.random.randint(0, 100, n),
    }
)

df.to_csv("comparison_data.csv", index=False)
df.to_json("comparison_data.json", orient="records")
df.to_parquet("comparison_data.parquet", index=False)
df.to_excel("comparison_data.xlsx", index=False, engine="openpyxl")

conn = sqlite3.connect("comparison_data.sqlite")
df.to_sql("data", conn, index=False, if_exists="replace")
conn.close()

print("wrote comparison_data.{csv,json,parquet,sqlite,xlsx}")
