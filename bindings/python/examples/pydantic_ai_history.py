"""Persist a PydanticAI conversation, without a model service or API key.

Optional dependency, tested with Python 3.12.15 and pydantic-ai-slim==2.56.0:
    python -m pip install 'pydantic-ai-slim==2.56.0'
    python examples/pydantic_ai_history.py

PydanticAI requires Python >=3.11; CedroDB's base SDK still supports >=3.9.
This recipe preserves messages, conversation identity, and accumulated usage
between completed runs. It does not resume an interrupted model/tool call or
implement a durable-execution backend. The application owns usage limits,
approvals, and trusted access to the stored conversation. Retest serialization
before upgrading the pinned optional dependency.
"""

from importlib.metadata import version
from pathlib import Path
from tempfile import TemporaryDirectory

from cedrodb import Database
from pydantic_ai import Agent, ConversationTypeAdapter
from pydantic_ai.models.test import TestModel


def main():
    # TestModel is deterministic and makes no network/model calls.
    model = TestModel(custom_output_text="The backup is stored locally.")
    agent = Agent(model)
    framework_version = version("pydantic-ai-slim")
    with TemporaryDirectory(prefix="cedrodb-pydantic-history-") as directory:
        path = Path(directory) / "chat.cedro"
        first = agent.run_sync("Where is the backup?", conversation_id="chat:1")
        with Database(path) as db:
            result = db.agent("pydantic-history").checkpoint(
                "chat:1",
                {"conversation": ConversationTypeAdapter.dump_python(first.conversation, mode="json")},
                metadata={"format": "pydantic-ai.conversation", "framework_version": framework_version},
                now_ms=1000,
            )
            assert result["applied"]
            revision = result["checkpoint"]["revision"]
        with Database(path) as reopened:
            store = reopened.agent("pydantic-history")
            checkpoint = store.get("chat:1")
            conversation = ConversationTypeAdapter.validate_python(checkpoint["state"]["conversation"])
            assert conversation.conversation_id == "chat:1"
            assert conversation.usage.requests == first.conversation.usage.requests
            assert len(conversation.messages) == len(first.all_messages())
            # A fresh agent receives the complete restored conversation bundle.
            next_agent = Agent(TestModel(custom_output_text="Yes. CedroDB stores it locally."))
            resumed = next_agent.run_sync("Can I use it offline?", conversation=conversation)
            assert len(resumed.all_messages()) > len(conversation.messages)
            assert resumed.conversation.conversation_id == conversation.conversation_id
            assert resumed.conversation.usage.requests > conversation.usage.requests
            saved = store.checkpoint(
                "chat:1",
                {"conversation": ConversationTypeAdapter.dump_python(resumed.conversation, mode="json")},
                expected_revision=checkpoint["revision"], status="completed",
                metadata=checkpoint["metadata"], now_ms=1001,
                operations=[{
                    "kind": "put", "collection": "outbox", "key": "chat:1:answered",
                    "value": {"event_id": "chat:1:answered", "type": "answer.ready"},
                }],
            )
            assert saved["applied"]
            assert saved["cursor"] > revision
            assert reopened.collection("outbox").get("chat:1:answered") is not None
            print({"framework_version": framework_version,
                   "conversation_id": resumed.conversation.conversation_id,
                   "history_messages": len(conversation.messages), "continued_messages": len(resumed.all_messages()),
                   "requests_before": conversation.usage.requests,
                   "requests_after": resumed.conversation.usage.requests,
                   "revision": saved["cursor"], "output": resumed.output})


if __name__ == "__main__":
    main()
