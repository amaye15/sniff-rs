import pandas as pd
from lib.util import helper
from . import config
import numpy as np

# df = pd.read_csv("ghost.csv")
df = pd.read_csv("data/sales.csv")
cust = pd.read_csv(os.path.join("..", "data", "customers.csv"))
df.to_csv("out/clean.csv", index=False)
with open("out/log.txt", "w") as log:
    log.write("done")
