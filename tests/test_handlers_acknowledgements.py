"""
Acknowledgements are a comment type, not a guess about comment text.

CheckMK types every comment: the Livestatus entry_type column is 1 for a user
comment, 2 for downtime, 3 for flapping and 4 for an acknowledgement. The ids
this listing returns are what remove_acknowledgement deletes by, so a listing
that includes an ordinary comment is a listing that can delete one.
"""

import pytest

from vibemk.handlers.acknowledgements import AcknowledgementHandler


def _comment(comment_id: str, text: str, persistent: bool = False) -> dict:
    return {
        "id": comment_id,
        "extensions": {"host_name": "web01", "comment": text, "persistent": persistent, "author": "ops"},
    }


@pytest.fixture
def handler(mock_checkmk_client):
    return AcknowledgementHandler(mock_checkmk_client)


class TestListAcknowledgements:
    @pytest.mark.asyncio
    async def test_asks_checkmk_for_the_acknowledgement_entry_type(self, handler):
        handler.client.get.return_value = {"success": True, "data": {"value": []}}

        await handler.list_acknowledgements({})

        params = handler.client.get.call_args.kwargs.get("params") or {}
        assert params.get("query") == {"op": "=", "left": "entry_type", "right": "4"}

    @pytest.mark.asyncio
    async def test_does_not_invent_acknowledgements_from_comment_text(self, handler):
        """CheckMK returned no acknowledgements, so the listing reports none.

        'packaging' contains 'ack'; under the old substring rule it qualified.
        """
        handler.client.get.return_value = {"success": True, "data": {"value": []}}

        result = await handler.list_acknowledgements({})

        assert "No active acknowledgements" in result[0]["text"]

    @pytest.mark.asyncio
    async def test_lists_what_checkmk_returns(self, handler):
        handler.client.get.return_value = {
            "success": True,
            "data": {"value": [_comment("42", "disk replaced, waiting for RMA")]},
        }

        result = await handler.list_acknowledgements({})

        assert "web01" in result[0]["text"]
        assert "disk replaced" in result[0]["text"]


class TestRemoveAcknowledgement:
    """Deleting the acknowledgement's comment leaves the problem acknowledged; the delete action clears it."""

    @pytest.mark.asyncio
    async def test_host_goes_through_the_delete_action(self, handler):
        result = await handler.remove_acknowledgement({"host_name": "web01"})

        handler.client.post.assert_called_once_with(
            "domain-types/acknowledge/actions/delete/invoke",
            data={"acknowledge_type": "host", "host_name": "web01"},
        )
        assert result[0]["text"] == "✅ Acknowledgement removed for web01"

    @pytest.mark.asyncio
    async def test_service_sends_its_description(self, handler):
        await handler.remove_acknowledgement({"host_name": "web01", "service_description": "PING"})

        assert handler.client.post.call_args.kwargs["data"] == {
            "acknowledge_type": "service",
            "host_name": "web01",
            "service_description": "PING",
        }

    @pytest.mark.asyncio
    async def test_an_id_names_the_service_its_comment_belongs_to(self, handler):
        service_ack = _comment("42", "disk replaced")
        service_ack["extensions"] |= {"is_service": True, "service_description": "Disk"}
        handler.client.get.return_value = {"success": True, "data": {"value": [_comment("7", "x"), service_ack]}}

        result = await handler.remove_acknowledgement({"acknowledgement_id": 42})

        assert handler.client.post.call_args.kwargs["data"]["service_description"] == "Disk"
        assert result[0]["text"] == "✅ Acknowledgement removed for web01/Disk"

    @pytest.mark.asyncio
    async def test_an_unknown_id_removes_nothing(self, handler):
        handler.client.get.return_value = {"success": True, "data": {"value": [_comment("7", "x")]}}

        result = await handler.remove_acknowledgement({"acknowledgement_id": "8"})

        handler.client.post.assert_not_called()
        assert "No acknowledgements found #8" in result[0]["text"]

    @pytest.mark.asyncio
    async def test_checkmk_without_the_action_says_so(self, handler):
        """CheckMK 2.3 answers 404 for the route: say there is no endpoint rather than claim success."""
        from vibemk.api import CheckMKNotFoundError

        handler.client.post.side_effect = CheckMKNotFoundError("Resource not found", 404)

        result = await handler.remove_acknowledgement({"host_name": "web01"})

        assert "no REST endpoint to remove an acknowledgement" in result[0]["text"]
