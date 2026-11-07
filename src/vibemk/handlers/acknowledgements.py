"""
CheckMK Acknowledgement Handler
Handles problem acknowledgements for hosts and services
"""

from typing import Any, Dict, List, Optional

from vibemk.api import CheckMKClient, CheckMKNotFoundError
from vibemk.handlers.base import BaseHandler
from vibemk.utils import get_logger

logger = get_logger(__name__)


ACKNOWLEDGEMENT_COMMENTS = "domain-types/comment/collections/all"
# Livestatus comments.entry_type: 1 user, 2 downtime, 3 flapping, 4 acknowledgement.
ACKNOWLEDGEMENT_ENTRY_TYPE = "4"
REMOVE_ACKNOWLEDGEMENT = "domain-types/acknowledge/actions/delete/invoke"
NO_REMOVE_ENDPOINT = (
    "❌ This CheckMK version has no REST endpoint to remove an acknowledgement\n\n"
    "Remove it in the GUI; a non-sticky acknowledgement also clears itself when the state changes."
)


class AcknowledgementHandler(BaseHandler):
    """Handler for CheckMK acknowledgement operations"""

    def __init__(self, client: CheckMKClient) -> None:
        super().__init__(client)
        self._checkmk_version: Optional[str] = None
        self._endpoints_tested = False

    async def _detect_checkmk_version(self) -> str:
        """Detect CheckMK version to use appropriate endpoints"""
        if self._checkmk_version is not None and self._endpoints_tested:
            return self._checkmk_version

        try:
            # Get version info from the API
            version_result = self.client.get("version")
            if version_result.get("success"):
                version_data = version_result.get("data", {})
                versions = version_data.get("versions", {})
                checkmk_version = versions.get("checkmk", "")

                if checkmk_version.startswith(("2.4", "2.5")):
                    self._checkmk_version = "2.4+"
                    logger.info(f"Detected CheckMK {checkmk_version} - using 2.4+ acknowledge endpoints")
                else:
                    self._checkmk_version = "2.3"
                    logger.info(f"Detected CheckMK {checkmk_version} - using 2.3 acknowledge endpoints")
            else:
                # Fallback: try server URL port to guess version
                server_url = self.client.config.server_url
                if ":8081" in server_url:
                    self._checkmk_version = "2.4+"
                    logger.info("Port 8081 detected - assuming CheckMK 2.4+")
                else:
                    self._checkmk_version = "2.3"
                    logger.info("Port 8080 detected - assuming CheckMK 2.3")

        except Exception as e:
            logger.warning(f"Could not detect CheckMK version, using port fallback: {e}")
            # Fallback: try server URL port to guess version
            try:
                server_url = self.client.config.server_url
                if ":8081" in server_url:
                    self._checkmk_version = "2.4+"
                    logger.info("Port 8081 detected - assuming CheckMK 2.4+")
                else:
                    self._checkmk_version = "2.3"
                    logger.info("Port 8080 detected - assuming CheckMK 2.3")
            except:
                self._checkmk_version = "2.3"

        self._endpoints_tested = True
        return self._checkmk_version

    async def handle(self, tool_name: str, arguments: Dict[str, Any]) -> List[Dict[str, str]]:
        """Route acknowledgement tool calls to appropriate methods"""
        method_map = {
            "vibemk_acknowledge_host_problem": self.acknowledge_host_problem,
            "vibemk_acknowledge_service_problem": self.acknowledge_service_problem,
            "vibemk_list_acknowledgements": self.list_acknowledgements,
            "vibemk_remove_acknowledgement": self.remove_acknowledgement,
        }

        if tool_name not in method_map:
            return self.error_text(f"❌ Unknown acknowledgement tool: {tool_name}")

        return await method_map[tool_name](arguments)

    async def acknowledge_host_problem(self, args: Dict[str, Any]) -> List[Dict[str, str]]:
        """Acknowledge a host problem"""
        try:
            host_name = args.get("host_name")
            comment = args.get("comment", "Problem acknowledged via vibeMK")
            sticky = args.get("sticky", False)
            persistent = args.get("persistent", False)
            notify = args.get("notify", False)
            expire_on = args.get("expire_on")  # Optional expiration time

            if not host_name:
                return self.error_text("❌ Error: host_name is required")

            # Detect CheckMK version and use appropriate endpoint
            version = await self._detect_checkmk_version()

            # Both CheckMK 2.3 and 2.4+ support the same endpoint
            endpoint = "domain-types/acknowledge/collections/host"

            data = {
                "acknowledge_type": "host",
                "sticky": sticky,
                "persistent": persistent,
                "notify": notify,
                "comment": comment,
                "host_name": host_name,
            }

            # Add expiration time if provided (2.4+ feature)
            if expire_on and version == "2.4+":
                data["expire_on"] = expire_on

            result = self.client.post(endpoint, data=data)

            if result.get("success"):
                expire_info = f"Expire on: {expire_on or 'Never'}\n" if version == "2.4+" else ""
                return [
                    {
                        "type": "text",
                        "text": f"✅ **Host Acknowledgement Created**\n\n"
                        f"Host: **{host_name}**\n"
                        f"Comment: {comment}\n"
                        f"Sticky: {sticky}\n"
                        f"Persistent: {persistent}\n"
                        f"Notify: {notify}\n"
                        f"{expire_info}\n"
                        f"CheckMK Version: {version}\n\n"
                        f"The host problem has been acknowledged and will not generate further alerts.",
                    }
                ]
            else:
                error_msg = result.get("data", {}).get("detail", "Unknown error")
                return self.error_text(f"❌ Failed to acknowledge host problem: {error_msg}")

        except Exception as e:
            logger.exception(f"Error acknowledging host problem for {host_name}")
            return self.error_text(f"❌ Error acknowledging host problem: {str(e)}")

    async def acknowledge_service_problem(self, args: Dict[str, Any]) -> List[Dict[str, str]]:
        """Acknowledge a service problem"""
        try:
            host_name = args.get("host_name")
            service_description = args.get("service_description")
            comment = args.get("comment", "Problem acknowledged via vibeMK")
            sticky = args.get("sticky", False)
            persistent = args.get("persistent", False)
            notify = args.get("notify", False)
            expire_on = args.get("expire_on")  # Optional expiration time

            if not host_name or not service_description:
                return self.error_text("❌ Error: host_name and service_description are required")

            # Detect CheckMK version and use appropriate endpoint
            version = await self._detect_checkmk_version()

            # Both CheckMK 2.3 and 2.4+ support the same endpoint
            endpoint = "domain-types/acknowledge/collections/service"

            data = {
                "acknowledge_type": "service",
                "sticky": sticky,
                "persistent": persistent,
                "notify": notify,
                "comment": comment,
                "host_name": host_name,
                "service_description": service_description,
            }

            # Add expiration time if provided (2.4+ feature)
            if expire_on and version == "2.4+":
                data["expire_on"] = expire_on

            result = self.client.post(endpoint, data=data)

            if result.get("success"):
                expire_info = f"Expire on: {expire_on or 'Never'}\n" if version == "2.4+" else ""
                return [
                    {
                        "type": "text",
                        "text": f"✅ **Service Acknowledgement Created**\n\n"
                        f"Host: **{host_name}**\n"
                        f"Service: **{service_description}**\n"
                        f"Comment: {comment}\n"
                        f"Sticky: {sticky}\n"
                        f"Persistent: {persistent}\n"
                        f"Notify: {notify}\n"
                        f"{expire_info}\n"
                        f"CheckMK Version: {version}\n\n"
                        f"The service problem has been acknowledged and will not generate further alerts.",
                    }
                ]
            else:
                error_msg = result.get("data", {}).get("detail", "Unknown error")
                return self.error_text(f"❌ Failed to acknowledge service problem: {error_msg}")

        except Exception as e:
            logger.exception(f"Error acknowledging service problem for {host_name}/{service_description}")
            return self.error_text(f"❌ Error acknowledging service problem: {str(e)}")

    def _remove_acknowledgement_of(self, host_name: str, service_description: Optional[str]) -> None:
        """Remove the acknowledgement of one host or service.

        Deleting the acknowledgement's comment leaves the problem acknowledged
        (measured on 2.3 and 2.5), so this goes through the dedicated action.
        CheckMK 2.3 lacks it and answers 404 for the route; an unknown host is a 400.
        """
        data: Dict[str, Any] = {
            "acknowledge_type": "service" if service_description else "host",
            "host_name": host_name,
        }
        if service_description:
            data["service_description"] = service_description
        self.client.post(REMOVE_ACKNOWLEDGEMENT, data=data)

    async def list_acknowledgements(self, args: Dict[str, Any]) -> List[Dict[str, str]]:
        """List all current acknowledgements"""
        try:
            # Acknowledgements are comments, and CheckMK types every comment:
            # the Livestatus entry_type column is 1 for a user comment, 2 for
            # downtime, 3 for flapping and 4 for an acknowledgement. Asking for
            # that type is exact. The previous rule guessed from the text -- it
            # took any comment containing "ack" as a substring, or merely marked
            # persistent -- so "track the vendor ticket" and "packaging" both
            # qualified, and those ids are what remove_acknowledgement deletes by.
            result = self.client.get(
                ACKNOWLEDGEMENT_COMMENTS,
                params={"query": {"op": "=", "left": "entry_type", "right": ACKNOWLEDGEMENT_ENTRY_TYPE}},
            )

            if not result.get("success"):
                return self.error_text("❌ Unable to retrieve acknowledgements from CheckMK API")

            acknowledgements = result.get("data", {}).get("value", [])

            if not acknowledgements:
                return [{"type": "text", "text": "✅ **Acknowledgements List**\n\nNo active acknowledgements found."}]

            # Format acknowledgements list
            ack_list = ["✅ **Current Acknowledgements**\n"]

            for i, ack in enumerate(acknowledgements, 1):
                extensions = ack.get("extensions", {})
                ack_id = ack.get("id", f"ack-{i}")
                host_name = extensions.get("host_name", "Unknown")
                is_service = extensions.get("is_service", False)
                comment = extensions.get("comment", "No comment")
                author = extensions.get("author", "Unknown")
                entry_time = extensions.get("entry_time", "Unknown")
                persistent = extensions.get("persistent", False)

                ack_type = "Service" if is_service else "Host"
                service_info = ""
                if is_service:
                    # For service acknowledgements, we might need to infer service name from comment
                    # or use additional API calls if needed
                    service_info = "\nType: Service acknowledgement"

                ack_list.append(
                    f"\n**{i}. Acknowledgement #{ack_id}**\n"
                    f"Type: {ack_type}\n"
                    f"Host: **{host_name}**{service_info}\n"
                    f"Comment: {comment}\n"
                    f"Author: {author}\n"
                    f"Entry time: {entry_time}\n"
                    f"Persistent: {persistent}\n"
                )

            return [{"type": "text", "text": "".join(ack_list)}]

        except Exception as e:
            logger.exception("Error listing acknowledgements")
            return self.error_text(f"❌ Error listing acknowledgements: {str(e)}")

    async def remove_acknowledgement(self, args: Dict[str, Any]) -> List[Dict[str, str]]:
        """Remove the acknowledgement of a host or service, or of those an id or a comment pattern names"""
        ack_id = args.get("acknowledgement_id")
        host_name = args.get("host_name")
        comment_pattern = args.get("comment_pattern")
        try:
            if ack_id or (comment_pattern and args.get("delete_all_matching", False)):
                # Ids and texts belong to the acknowledgement comments; each names the host or service to clear.
                listed = self.client.get(
                    ACKNOWLEDGEMENT_COMMENTS,
                    params={"query": {"op": "=", "left": "entry_type", "right": ACKNOWLEDGEMENT_ENTRY_TYPE}},
                )
                found = [(c.get("id"), c.get("extensions", {})) for c in listed.get("data", {}).get("value", [])]
                if ack_id:
                    matches = [e for i, e in found if str(i) == str(ack_id)]
                    label = f"#{ack_id}"
                else:
                    matches = [e for _, e in found if str(comment_pattern).lower() in str(e.get("comment", "")).lower()]
                    label = f"matching pattern '{comment_pattern}'"
                targets = {
                    (e.get("host_name"), e.get("service_description") if e.get("is_service") else None) for e in matches
                }
                if not targets:
                    return self.error_text(f"❌ No acknowledgements found {label}")
            elif host_name:
                targets = {(host_name, args.get("service_description") or None)}
            else:
                return self.error_text("❌ Error: acknowledgement_id, host_name, or comment_pattern is required")

            ordered = sorted(targets, key=lambda t: (str(t[0]), t[1] or ""))
            for host, service in ordered:
                self._remove_acknowledgement_of(host, service)
        except CheckMKNotFoundError:
            return self.error_text(NO_REMOVE_ENDPOINT)
        except Exception as e:
            logger.exception("Error removing acknowledgement")
            return self.error_text(f"❌ Error removing acknowledgement: {str(e)}")

        names = ", ".join(host if service is None else f"{host}/{service}" for host, service in ordered)
        return [{"type": "text", "text": f"✅ Acknowledgement removed for {names}"}]
