"""Document mutations, bounded queries and exact retrieval."""
from .. import MISSING
from .errors import CliUsageError


def execute(db, args, payload):
    if args.command == "batch":
        if not isinstance(payload, list):
            raise CliUsageError("batch input must be a JSON array of operations")
        db.batch(payload)
        return {"committed": True, "operations": len(payload)}, 0
    collection = db.collection(args.collection)
    if args.command == "put":
        collection.put(args.key, payload)
        return {"written": True, "collection": args.collection, "key": args.key}, 0
    if args.command == "get":
        value = collection.get(args.key, MISSING)
        if value is MISSING:
            return {"found": False, "key": args.key}, 3
        return {"found": True, "key": args.key, "value": value}, 0
    if args.command == "delete":
        existed = collection.get(args.key, MISSING) is not MISSING
        if existed:
            collection.delete(args.key)
        return {"deleted": existed, "key": args.key}, 0
    for field in args.index:
        collection.create_index(field)
    if args.command in ("find", "query", "profile-query"):
        call = collection.profile_query if args.command == "profile-query" else collection.find_page
        return call(args.filter, limit=args.limit, after=args.after), 0
    call = collection.profile_search if args.command == "profile-search" else collection.search
    return call(text=args.text, vector=args.vector, vector_field=args.vector_field,
                text_fields=args.text_field, filters=args.filter, limit=args.limit,
                max_scanned=args.max_scanned, min_vector_score=args.min_vector_score), 0
