export * from './index.js';
import { Database, Collection, Queue, AgentStore, version } from './index.js';
declare const cedrodb: { Database: typeof Database; Collection: typeof Collection; Queue: typeof Queue; AgentStore: typeof AgentStore; version: typeof version };
export default cedrodb;
