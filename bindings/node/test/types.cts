import cedrodb = require('cedrodb');
import { Database, type Query } from 'cedrodb';
const db: cedrodb.Database = new cedrodb.Database('./unused-typecheck');
const other = new Database('./unused-typecheck-other');
const query: Query = { limit: 2 };
const result: cedrodb.QueryPage = db.collection('c').findPage(query);
const version: '0.2.0' = cedrodb.version;
void [other, result, version];
