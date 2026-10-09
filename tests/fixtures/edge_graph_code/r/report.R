source("etl/helper.R")
d <- read.csv("out/clean.csv")
write.csv(d, "out/report.csv")
