//! Port of src/vibemk/server/registry.py: MCP tool name -> handler.
//! Knows nothing about JSON-RPC or transport.

use std::collections::HashMap;
use std::sync::Arc;

use crate::client::CheckMkClient;
use crate::handlers::base::Handler;
use crate::handlers::{
    AcknowledgementHandler, ActiveChecksHandler, AgentHandler, AuditLogHandler, AuxTagsHandler,
    ConfigurationHandler, ConnectionHandler, DebugHandler, DiscoveryHandler, DowntimeHandler,
    EventConsoleHandler, FolderHandler, GroupsHandler, HostGroupRulesHandler, HostHandler,
    MetricsHandler, MonitoringHandler, NotificationHandler, PasswordsHandler, RulesHandler,
    RulesetsHandler, ServiceGroupHandler, ServiceHandler, ServiceParamsHandler, SitesHandler,
    TagsHandler, TimePeriodsHandler, UserHandler, UserRolesHandler,
};

pub struct ToolRegistry {
    handlers: HashMap<&'static str, Arc<dyn Handler>>,
}

impl ToolRegistry {
    pub fn new(handlers: HashMap<&'static str, Arc<dyn Handler>>) -> Self {
        Self { handlers }
    }

    /// Build every handler against one CheckMK client.
    pub fn from_client(client: Arc<CheckMkClient>) -> Self {
        let connection_handler: Arc<dyn Handler> = Arc::new(ConnectionHandler::new(client.clone()));
        let host_handler: Arc<dyn Handler> = Arc::new(HostHandler::new(client.clone()));
        let service_handler: Arc<dyn Handler> = Arc::new(ServiceHandler::new(client.clone()));
        let monitoring_handler: Arc<dyn Handler> = Arc::new(MonitoringHandler::new(client.clone()));
        let notification_handler: Arc<dyn Handler> =
            Arc::new(NotificationHandler::new(client.clone()));
        let configuration_handler: Arc<dyn Handler> =
            Arc::new(ConfigurationHandler::new(client.clone()));
        let folder_handler: Arc<dyn Handler> = Arc::new(FolderHandler::new(client.clone()));
        let metrics_handler: Arc<dyn Handler> = Arc::new(MetricsHandler::new(client.clone()));
        let user_handler: Arc<dyn Handler> = Arc::new(UserHandler::new(client.clone()));
        let user_roles_handler: Arc<dyn Handler> = Arc::new(UserRolesHandler::new(client.clone()));
        let groups_handler: Arc<dyn Handler> = Arc::new(GroupsHandler::new(client.clone()));
        let rules_handler: Arc<dyn Handler> = Arc::new(RulesHandler::new(client.clone()));
        let rulesets_handler: Arc<dyn Handler> = Arc::new(RulesetsHandler::new(client.clone()));
        let tags_handler: Arc<dyn Handler> = Arc::new(TagsHandler::new(client.clone()));
        let timeperiods_handler: Arc<dyn Handler> =
            Arc::new(TimePeriodsHandler::new(client.clone()));
        let passwords_handler: Arc<dyn Handler> = Arc::new(PasswordsHandler::new(client.clone()));
        let debug_handler: Arc<dyn Handler> = Arc::new(DebugHandler::new(client.clone()));
        let host_group_rules_handler: Arc<dyn Handler> =
            Arc::new(HostGroupRulesHandler::new(client.clone()));
        let downtime_handler: Arc<dyn Handler> = Arc::new(DowntimeHandler::new(client.clone()));
        let acknowledgement_handler: Arc<dyn Handler> =
            Arc::new(AcknowledgementHandler::new(client.clone()));
        let discovery_handler: Arc<dyn Handler> = Arc::new(DiscoveryHandler::new(client.clone()));
        let service_group_handler: Arc<dyn Handler> =
            Arc::new(ServiceGroupHandler::new(client.clone()));
        let active_checks_handler: Arc<dyn Handler> =
            Arc::new(ActiveChecksHandler::new(client.clone()));
        let agent_handler: Arc<dyn Handler> = Arc::new(AgentHandler::new(client.clone()));
        let audit_log_handler: Arc<dyn Handler> = Arc::new(AuditLogHandler::new(client.clone()));
        let aux_tags_handler: Arc<dyn Handler> = Arc::new(AuxTagsHandler::new(client.clone()));
        let event_console_handler: Arc<dyn Handler> =
            Arc::new(EventConsoleHandler::new(client.clone()));
        let service_params_handler: Arc<dyn Handler> =
            Arc::new(ServiceParamsHandler::new(client.clone()));
        let sites_handler: Arc<dyn Handler> = Arc::new(SitesHandler::new(client.clone()));

        let entries: Vec<(&'static str, Arc<dyn Handler>)> = vec![
            // Connection tools
            (
                "vibemk_debug_checkmk_connection",
                connection_handler.clone(),
            ),
            ("vibemk_debug_url_detection", connection_handler.clone()),
            ("vibemk_test_direct_url", connection_handler.clone()),
            ("vibemk_test_all_endpoints", connection_handler.clone()),
            ("vibemk_get_checkmk_version", connection_handler.clone()),
            // Host management tools
            ("vibemk_get_checkmk_hosts", host_handler.clone()),
            ("vibemk_get_host_status", host_handler.clone()),
            ("vibemk_get_host_details", host_handler.clone()),
            ("vibemk_get_host_config", host_handler.clone()),
            ("vibemk_create_host", host_handler.clone()),
            ("vibemk_bulk_create_hosts", host_handler.clone()),
            ("vibemk_update_host", host_handler.clone()),
            ("vibemk_delete_host", host_handler.clone()),
            ("vibemk_move_host", host_handler.clone()),
            ("vibemk_bulk_update_hosts", host_handler.clone()),
            ("vibemk_create_cluster_host", host_handler.clone()),
            ("vibemk_validate_host_config", host_handler.clone()),
            ("vibemk_compare_host_states", host_handler.clone()),
            ("vibemk_get_host_effective_attributes", host_handler.clone()),
            // Service management tools
            ("vibemk_get_checkmk_services", service_handler.clone()),
            ("vibemk_get_service_status", service_handler.clone()),
            // Monitoring and problems
            ("vibemk_get_current_problems", monitoring_handler.clone()),
            ("vibemk_acknowledge_problem", monitoring_handler.clone()),
            ("vibemk_schedule_downtime", downtime_handler.clone()),
            ("vibemk_get_downtimes", monitoring_handler.clone()),
            ("vibemk_get_comments", monitoring_handler.clone()),
            ("vibemk_add_comment", monitoring_handler.clone()),
            // Configuration management
            ("vibemk_activate_changes", configuration_handler.clone()),
            ("vibemk_get_pending_changes", configuration_handler.clone()),
            // Folder management
            ("vibemk_get_folders", folder_handler.clone()),
            ("vibemk_create_folder", folder_handler.clone()),
            ("vibemk_delete_folder", folder_handler.clone()),
            ("vibemk_update_folder", folder_handler.clone()),
            ("vibemk_move_folder", folder_handler.clone()),
            ("vibemk_get_folder_hosts", folder_handler.clone()),
            // Metrics and performance data (RRD access)
            ("vibemk_get_host_metrics", metrics_handler.clone()),
            ("vibemk_get_service_metrics", metrics_handler.clone()),
            ("vibemk_list_available_metrics", metrics_handler.clone()),
            // User management
            ("vibemk_get_users", user_handler.clone()),
            ("vibemk_create_user", user_handler.clone()),
            ("vibemk_update_user", user_handler.clone()),
            ("vibemk_delete_user", user_handler.clone()),
            ("vibemk_get_contact_groups", user_handler.clone()),
            ("vibemk_create_contact_group", user_handler.clone()),
            ("vibemk_update_contact_group", user_handler.clone()),
            ("vibemk_delete_contact_group", user_handler.clone()),
            ("vibemk_add_user_to_group", user_handler.clone()),
            ("vibemk_remove_user_from_group", user_handler.clone()),
            // User roles management
            ("vibemk_list_user_roles", user_roles_handler.clone()),
            ("vibemk_show_user_role", user_roles_handler.clone()),
            ("vibemk_create_user_role", user_roles_handler.clone()),
            ("vibemk_update_user_role", user_roles_handler.clone()),
            ("vibemk_delete_user_role", user_roles_handler.clone()),
            // Group management (host and service groups)
            ("vibemk_get_host_groups", groups_handler.clone()),
            ("vibemk_create_host_group", groups_handler.clone()),
            ("vibemk_update_host_group", groups_handler.clone()),
            ("vibemk_delete_host_group", groups_handler.clone()),
            ("vibemk_get_service_groups", groups_handler.clone()),
            // Rule management
            ("vibemk_get_rulesets", rules_handler.clone()),
            ("vibemk_get_ruleset", rules_handler.clone()),
            ("vibemk_create_rule", rules_handler.clone()),
            ("vibemk_update_rule", rules_handler.clone()),
            ("vibemk_delete_rule", rules_handler.clone()),
            ("vibemk_move_rule", rules_handler.clone()),
            // Ruleset discovery and search
            ("vibemk_search_rulesets", rulesets_handler.clone()),
            ("vibemk_show_ruleset", rulesets_handler.clone()),
            ("vibemk_list_rulesets", rulesets_handler.clone()),
            // Tag management (host tags)
            ("vibemk_get_host_tags", tags_handler.clone()),
            ("vibemk_create_host_tag", tags_handler.clone()),
            ("vibemk_update_host_tag", tags_handler.clone()),
            ("vibemk_delete_host_tag", tags_handler.clone()),
            // Time period management
            ("vibemk_get_timeperiods", timeperiods_handler.clone()),
            ("vibemk_create_timeperiod", timeperiods_handler.clone()),
            ("vibemk_update_timeperiod", timeperiods_handler.clone()),
            ("vibemk_delete_timeperiod", timeperiods_handler.clone()),
            // Password management
            ("vibemk_get_passwords", passwords_handler.clone()),
            ("vibemk_create_password", passwords_handler.clone()),
            ("vibemk_update_password", passwords_handler.clone()),
            ("vibemk_delete_password", passwords_handler.clone()),
            // Notification rules
            (
                "vibemk_get_notification_rules",
                notification_handler.clone(),
            ),
            ("vibemk_get_notification_rule", notification_handler.clone()),
            (
                "vibemk_create_notification_rule",
                notification_handler.clone(),
            ),
            (
                "vibemk_update_notification_rule",
                notification_handler.clone(),
            ),
            (
                "vibemk_delete_notification_rule",
                notification_handler.clone(),
            ),
            // Debug tools
            ("vibemk_debug_api_endpoints", debug_handler.clone()),
            ("vibemk_debug_permissions", debug_handler.clone()),
            // Host group rules
            (
                "vibemk_find_host_grouping_rulesets",
                host_group_rules_handler.clone(),
            ),
            (
                "vibemk_create_host_contactgroup_rule",
                host_group_rules_handler.clone(),
            ),
            (
                "vibemk_create_host_hostgroup_rule",
                host_group_rules_handler.clone(),
            ),
            (
                "vibemk_get_example_rule_structures",
                host_group_rules_handler.clone(),
            ),
            // Downtime management
            ("vibemk_schedule_host_downtime", downtime_handler.clone()),
            ("vibemk_schedule_service_downtime", downtime_handler.clone()),
            ("vibemk_list_downtimes", downtime_handler.clone()),
            ("vibemk_get_active_downtimes", downtime_handler.clone()),
            ("vibemk_delete_downtime", downtime_handler.clone()),
            (
                "vibemk_check_host_downtime_status",
                downtime_handler.clone(),
            ),
            // Acknowledgement management
            (
                "vibemk_acknowledge_host_problem",
                acknowledgement_handler.clone(),
            ),
            (
                "vibemk_acknowledge_service_problem",
                acknowledgement_handler.clone(),
            ),
            (
                "vibemk_list_acknowledgements",
                acknowledgement_handler.clone(),
            ),
            (
                "vibemk_remove_acknowledgement",
                acknowledgement_handler.clone(),
            ),
            // Discovery management
            ("vibemk_start_service_discovery", discovery_handler.clone()),
            ("vibemk_start_bulk_discovery", discovery_handler.clone()),
            ("vibemk_get_discovery_status", discovery_handler.clone()),
            (
                "vibemk_get_bulk_discovery_status",
                discovery_handler.clone(),
            ),
            ("vibemk_wait_for_discovery", discovery_handler.clone()),
            (
                "vibemk_get_discovery_background_job",
                discovery_handler.clone(),
            ),
            // Service group management
            ("vibemk_create_service_group", service_group_handler.clone()),
            ("vibemk_list_service_groups", service_group_handler.clone()),
            ("vibemk_get_service_group", service_group_handler.clone()),
            ("vibemk_update_service_group", service_group_handler.clone()),
            ("vibemk_delete_service_group", service_group_handler.clone()),
            (
                "vibemk_bulk_create_service_groups",
                service_group_handler.clone(),
            ),
            (
                "vibemk_bulk_update_service_groups",
                service_group_handler.clone(),
            ),
            (
                "vibemk_bulk_delete_service_groups",
                service_group_handler.clone(),
            ),
            // Active checks (PR #3)
            ("vibemk_create_custom_check", active_checks_handler.clone()),
            ("vibemk_create_dns_check", active_checks_handler.clone()),
            ("vibemk_create_ftp_check", active_checks_handler.clone()),
            ("vibemk_create_http_check", active_checks_handler.clone()),
            ("vibemk_create_icmp_check", active_checks_handler.clone()),
            (
                "vibemk_create_inventory_check",
                active_checks_handler.clone(),
            ),
            ("vibemk_create_ldap_check", active_checks_handler.clone()),
            (
                "vibemk_create_mkevents_check",
                active_checks_handler.clone(),
            ),
            ("vibemk_create_smb_check", active_checks_handler.clone()),
            ("vibemk_create_smtp_check", active_checks_handler.clone()),
            ("vibemk_create_tcp_check", active_checks_handler.clone()),
            ("vibemk_delete_active_check", active_checks_handler.clone()),
            ("vibemk_list_active_checks", active_checks_handler.clone()),
            // Agent bakery (CEE/Cloud)
            ("vibemk_bake_agents", agent_handler.clone()),
            ("vibemk_baking_status", agent_handler.clone()),
            ("vibemk_download_agent_by_host", agent_handler.clone()),
            // Audit log
            ("vibemk_get_audit_log", audit_log_handler.clone()),
            // Auxiliary tags
            ("vibemk_create_aux_tag", aux_tags_handler.clone()),
            ("vibemk_delete_aux_tag", aux_tags_handler.clone()),
            ("vibemk_get_aux_tags", aux_tags_handler.clone()),
            ("vibemk_update_aux_tag", aux_tags_handler.clone()),
            // Event console
            ("vibemk_acknowledge_event", event_console_handler.clone()),
            ("vibemk_change_event_state", event_console_handler.clone()),
            ("vibemk_delete_events", event_console_handler.clone()),
            ("vibemk_get_events", event_console_handler.clone()),
            // Service parameters
            (
                "vibemk_delete_service_param_rule",
                service_params_handler.clone(),
            ),
            ("vibemk_list_process_rules", service_params_handler.clone()),
            (
                "vibemk_set_interface_params",
                service_params_handler.clone(),
            ),
            (
                "vibemk_set_memory_thresholds",
                service_params_handler.clone(),
            ),
            (
                "vibemk_set_process_thresholds",
                service_params_handler.clone(),
            ),
            // Sites
            ("vibemk_get_sites", sites_handler.clone()),
            ("vibemk_login_site", sites_handler.clone()),
            ("vibemk_logout_site", sites_handler.clone()),
            // Further tools added alongside the handlers above
            ("vibemk_checkmk_rules_guide", configuration_handler.clone()),
            ("vibemk_modify_downtime", downtime_handler.clone()),
            ("vibemk_clone_host", host_handler.clone()),
            ("vibemk_rename_host", host_handler.clone()),
            ("vibemk_get_custom_graph", metrics_handler.clone()),
            ("vibemk_search_metrics", metrics_handler.clone()),
            ("vibemk_delete_comment", monitoring_handler.clone()),
        ];
        Self::new(entries.into_iter().collect())
    }

    /// The handler for a tool, or None when it is not registered.
    pub fn handler_for(&self, tool_name: &str) -> Option<Arc<dyn Handler>> {
        self.handlers.get(tool_name).cloned()
    }

    /// Every tool name this registry can route.
    pub fn tool_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.handlers.keys().copied().collect();
        names.sort_unstable();
        names
    }
}
