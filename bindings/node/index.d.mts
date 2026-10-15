export * from './index.js';
import { Database, Collection, Queue, version } from './index.js';
declare const cedrodb: { Database: typeof Database; Collection: typeof Collection; Queue: typeof Queue; version: typeof version };
export default cedrodb;
