import { add } from './lib/math.js';
const fs = require('fs');
const rows = fs.readFileSync('../data/sales.csv', 'utf8');
fs.writeFileSync('out/clean.csv', rows);
