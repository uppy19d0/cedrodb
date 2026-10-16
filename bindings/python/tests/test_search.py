import math
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from cedrodb import Database, CedroError, DatabaseClosedError


class SearchTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-search-")
        self.path = Path(self.directory.name) / "search.cedro"
        self.db = Database(self.path)
        self.documents = self.db.collection("knowledge")
        self.documents.put_many({
            "a": {"text": "cedro local database", "embedding": [1, 0], "owner": "ana"},
            "b": {"text": "local local agent", "embedding": [0.8, 0.6], "owner": "ana"},
            "c": {"text": "remote archive", "embedding": [-1, 0], "owner": "luis"},
            "d": {"text": "cedro", "owner": "ana"},
        })

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def test_exact_cosine_ranking_threshold_and_ties(self):
        page = self.documents.search(vector=[1, 0])
        self.assertEqual(page["mode"], "vector")
        self.assertEqual([hit["key"] for hit in page["hits"]], ["a", "b", "c"])
        for hit, expected in zip(page["hits"], [1, 0.8, -1]):
            self.assertAlmostEqual(hit["vector_score"], expected)
            self.assertAlmostEqual(hit["score"], expected)
            self.assertIsNone(hit["text_score"])
        threshold = self.documents.search(vector=[1, 0], min_vector_score=0.9)
        self.assertEqual([hit["key"] for hit in threshold["hits"]], ["a"])
        self.documents.put("aa", {"embedding": [1, 0]})
        self.assertEqual([hit["key"] for hit in self.documents.search(vector=[1, 0], limit=2)["hits"]], ["a", "aa"])

    def test_bm25_unicode_casefold_and_hybrid_rrf(self):
        self.documents.put("accent", {"text": "ÁRBOL árbol", "embedding": [0, 1]})
        text = self.documents.search(text="Árbol")
        self.assertEqual(text["mode"], "text")
        self.assertEqual([hit["key"] for hit in text["hits"]], ["accent"])
        self.assertGreater(text["hits"][0]["text_score"], 0)
        self.assertIsNone(text["hits"][0]["vector_score"])
        hybrid = self.documents.search(text="local", vector=[1, 0], limit=2)
        self.assertEqual(hybrid["mode"], "hybrid")
        self.assertEqual({hit["key"] for hit in hybrid["hits"]}, {"a", "b"})
        self.assertTrue(all(0 < hit["score"] <= 2 / 61 for hit in hybrid["hits"]))

    def test_metadata_prefilter_precedes_document_vector_validation(self):
        self.documents.put("bad-other-owner", {"owner": "luis", "embedding": [0, 0]})
        filters = [{"field": "owner", "op": "eq", "value": "ana"}]
        expected = self.documents.search(vector=[1, 0], filters=filters)
        self.assertEqual([hit["key"] for hit in expected["hits"]], ["a", "b"])
        self.documents.create_index("owner")
        indexed = self.documents.search(vector=[1, 0], filters=filters)
        self.assertEqual(indexed["hits"], expected["hits"])
        self.assertEqual(indexed["used_index"], "owner")
        with self.assertRaises(CedroError):
            self.documents.search(vector=[1, 0])

    def test_budget_raises_without_silent_truncation_and_index_bounds_candidates(self):
        before = self.db.stats()
        with self.assertRaises(CedroError):
            self.documents.search(text="local", max_scanned=3)
        self.assertEqual(self.db.stats(), before)
        self.documents.create_index("owner")
        indexed = self.documents.search(text="local", max_scanned=3, filters=[
            {"field": "owner", "op": "eq", "value": "ana"},
        ])
        self.assertEqual(indexed["used_index"], "owner")
        self.assertEqual(indexed["scanned"], 3)
        self.assertEqual({hit["key"] for hit in indexed["hits"]}, {"a", "b"})

    def test_custom_pointer_fields_and_reopen_are_equivalent(self):
        self.documents.put("nested", {"content": {"body": "tool execution", "vec": [1, 0]}})
        query = dict(text="execution", vector=[1, 0], vector_field="/content/vec", text_fields=["/content/body"])
        before = self.documents.search(**query)
        self.assertEqual([hit["key"] for hit in before["hits"]], ["nested"])
        self.db.close()
        self.db = Database(self.path)
        self.documents = self.db.collection("knowledge")
        self.assertEqual(self.documents.search(**query), before)

    def test_invalid_vectors_and_query_options_fail_without_mutation(self):
        before = self.db.stats()
        invalid = [
            {}, {"vector": []}, {"vector": [0, 0]}, {"vector": [True, 0]},
            {"vector": [math.nan, 0]}, {"vector": [math.inf, 0]}, {"vector": "12"},
            {"vector": [1, 0], "limit": True}, {"vector": [1, 0], "limit": 0},
            {"vector": [1, 0], "limit": 1001}, {"text": 1},
            {"text": "local", "max_scanned": False}, {"text": "local", "max_scanned": 0},
            {"text": "local", "max_scanned": 100001}, {"text": "local", "text_fields": "text"},
            {"text": "local", "text_fields": [False]}, {"vector": [1, 0], "min_vector_score": True},
            {"vector": [1, 0], "min_vector_score": math.nan},
        ]
        for query in invalid:
            with self.subTest(query=query):
                with self.assertRaises((CedroError, ValueError, TypeError)):
                    self.documents.search(**query)
                self.assertEqual(self.db.stats(), before)
        self.documents.put("wrong-dimension", {"embedding": [1, 0, 0]})
        with self.assertRaises(CedroError):
            self.documents.search(vector=[1, 0])

    def test_missing_vectors_skip_but_invalid_present_vectors_raise(self):
        for vector in [None, "invalid", [0, 0], [1, "bad"]]:
            self.documents.put("invalid", {"embedding": vector})
            with self.subTest(vector=vector):
                with self.assertRaises(CedroError):
                    self.documents.search(vector=[1, 0])
        self.documents.delete("invalid")
        self.assertEqual(len(self.documents.search(vector=[1, 0])["hits"]), 3)

    def test_closed_collection_handle_fails(self):
        self.db.close()
        with self.assertRaises(DatabaseClosedError):
            self.documents.search(text="local")


if __name__ == "__main__":
    unittest.main()
