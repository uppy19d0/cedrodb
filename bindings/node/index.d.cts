import type * as API from './index.js';
declare namespace cedrodb {
  const Database: typeof API.Database;
  type Database = API.Database;
  const Collection: typeof API.Collection;
  type Collection<T extends API.JsonValue = API.JsonValue> = API.Collection<T>;
  const version: typeof API.version;
  type JsonValue = API.JsonValue;
  type DatabaseOptions = API.DatabaseOptions;
  type Record<T extends API.JsonValue = API.JsonValue> = API.Record<T>;
  type FilterOp = API.FilterOp;
  type Filter = API.Filter;
  type Query = API.Query;
  type QueryPage<T extends API.JsonValue = API.JsonValue> = API.QueryPage<T>;
  type BatchOperation = API.BatchOperation;
  type ChangeTransaction = API.ChangeTransaction;
  type ChangePage = API.ChangePage;
  type Stats = API.Stats;
  type Aggregate = API.Aggregate;
}
export = cedrodb;
