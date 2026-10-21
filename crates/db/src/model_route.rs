//! Persistence for reusable model routes and the Agents that reference them.
//!
//! A route is stored as one row plus its members. The members are read back in
//! attempt order, and every write replaces the whole member list: reordering is
//! a different list, not a different field, so a partial update would leave the
//! order ambiguous.
//!
//! Bindings follow the same shape as Provider bindings: a live row per
//! `(Agent, route)` pair, soft-deleted rather than removed so an Agent's
//! history survives a route being unbound.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use vibex_core::{
    AgentId, AgentModelRouteBinding, AgentModelRouteBindingId, ModelProviderProfileId, ModelRoute,
    ModelRouteId, ModelRouteMember, ModelRouteMemberId, RouteKind, RouteStatus, RouteStrategy,
    VibexError, VibexResult,
};

use super::{enum_from_db_sql, enum_to_db, parse_id_sql, storage_err, unix_timestamp_ms};

pub struct ModelRouteRepository;
pub struct AgentModelRouteBindingRepository;

impl ModelRouteRepository {
    pub fn insert(conn: &Connection, route: &ModelRoute) -> VibexResult<()> {
        route.validate()?;
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "model_route_create_transaction_failed",
                "failed to start model route transaction",
            ))?;
        insert_route(&transaction, route)?;
        insert_members(&transaction, route)?;
        transaction.commit().map_err(storage_err(
            "model_route_create_commit_failed",
            "failed to commit model route",
        ))
    }

    pub fn get(conn: &Connection, id: &ModelRouteId) -> VibexResult<Option<ModelRoute>> {
        let mut route = conn
            .query_row(
                "
                SELECT model_route_id, display_name, description, kind, strategy,
                    first_token_timeout_ms, context_tokens, status, revision,
                    created_at_ms, updated_at_ms, deleted_at_ms
                FROM model_routes
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![id.as_str()],
                map_route,
            )
            .optional()
            .map_err(storage_err(
                "model_route_lookup_failed",
                "failed to load model route",
            ))?;
        if let Some(route) = route.as_mut() {
            route.members = list_members(conn, &route.id)?;
        }
        Ok(route)
    }

    /// Every live route, most recently changed first.
    pub fn list(conn: &Connection) -> VibexResult<Vec<ModelRoute>> {
        list_where(conn, "deleted_at_ms IS NULL", "")
    }

    /// Every live route that is enabled, in name order.
    ///
    /// This is what a projection reads: a disabled route still exists, but it
    /// offers no model to an Agent.
    pub fn list_enabled(conn: &Connection) -> VibexResult<Vec<ModelRoute>> {
        list_where(
            conn,
            "deleted_at_ms IS NULL AND status = 'enabled'",
            "ORDER BY display_name COLLATE NOCASE ASC, model_route_id ASC",
        )
    }

    /// Replaces every field of a route, including its member list.
    ///
    /// The caller must pass the revision it read; a mismatch means someone else
    /// changed the route in between. Members are replaced wholesale, so the
    /// order that arrives is the order that is stored.
    pub fn update(
        conn: &Connection,
        route: &ModelRoute,
        expected_revision: i64,
    ) -> VibexResult<ModelRoute> {
        route.validate()?;
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "model_route_update_transaction_failed",
                "failed to start model route update transaction",
            ))?;
        let changed = transaction
            .execute(
                "
                UPDATE model_routes
                SET display_name = ?2,
                    description = ?3,
                    kind = ?4,
                    strategy = ?5,
                    first_token_timeout_ms = ?6,
                    context_tokens = ?7,
                    status = ?8,
                    revision = revision + 1,
                    updated_at_ms = ?9
                WHERE model_route_id = ?1
                    AND revision = ?10
                    AND deleted_at_ms IS NULL
                ",
                params![
                    route.id.as_str(),
                    route.display_name.trim(),
                    route.description.as_deref().map(str::trim),
                    enum_to_db(&route.kind)?,
                    enum_to_db(&route.strategy)?,
                    route.first_token_timeout_ms,
                    route.context_tokens,
                    enum_to_db(&route.status)?,
                    next_stamp(conn, route)?,
                    expected_revision,
                ],
            )
            .map_err(storage_err(
                "model_route_update_failed",
                "failed to update model route",
            ))?;
        if changed == 0 {
            return Err(VibexError::validation(
                "model_route_revision_conflict",
                "the route changed since it was loaded",
            )
            .with_diagnostic("modelRouteId", route.id.as_str()));
        }
        transaction
            .execute(
                "DELETE FROM model_route_members WHERE model_route_id = ?1",
                params![route.id.as_str()],
            )
            .map_err(storage_err(
                "model_route_members_replace_failed",
                "failed to replace model route members",
            ))?;
        insert_members(&transaction, route)?;
        transaction.commit().map_err(storage_err(
            "model_route_update_commit_failed",
            "failed to commit model route update",
        ))?;
        Self::get(conn, &route.id)?.ok_or_else(|| {
            VibexError::storage(
                "model_route_update_readback_failed",
                "failed to read the model route after update",
            )
        })
    }

    /// Marks a route deleted. Its bindings go with it.
    pub fn delete(conn: &Connection, id: &ModelRouteId) -> VibexResult<bool> {
        let now = unix_timestamp_ms();
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "model_route_delete_transaction_failed",
                "failed to start model route delete transaction",
            ))?;
        let changed = transaction
            .execute(
                "
                UPDATE model_routes
                SET deleted_at_ms = ?2,
                    revision = revision + 1,
                    updated_at_ms = ?2
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![id.as_str(), now],
            )
            .map_err(storage_err(
                "model_route_delete_failed",
                "failed to delete model route",
            ))?;
        transaction
            .execute(
                "
                UPDATE agent_model_route_bindings
                SET deleted_at_ms = ?2,
                    is_default = 0,
                    revision = revision + 1,
                    updated_at_ms = ?2
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![id.as_str(), now],
            )
            .map_err(storage_err(
                "model_route_bindings_delete_failed",
                "failed to delete the route's Agent bindings",
            ))?;
        transaction.commit().map_err(storage_err(
            "model_route_delete_commit_failed",
            "failed to commit model route delete",
        ))?;
        Ok(changed > 0)
    }

    /// Every live route that names this Provider, so a Provider change can
    /// report what it would break before it happens.
    pub fn list_naming_provider(
        conn: &Connection,
        provider: &ModelProviderProfileId,
    ) -> VibexResult<Vec<ModelRoute>> {
        let ids = {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT DISTINCT m.model_route_id
                    FROM model_route_members m
                    JOIN model_routes r ON r.model_route_id = m.model_route_id
                    WHERE m.model_provider_profile_id = ?1 AND r.deleted_at_ms IS NULL
                    ",
                )
                .map_err(storage_err(
                    "model_route_provider_lookup_failed",
                    "failed to list routes naming a Provider",
                ))?;
            let rows = stmt
                .query_map(params![provider.as_str()], |row| row.get::<_, String>(0))
                .map_err(storage_err(
                    "model_route_provider_lookup_failed",
                    "failed to list routes naming a Provider",
                ))?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(storage_err(
                    "model_route_provider_lookup_failed",
                    "failed to list routes naming a Provider",
                ))?);
            }
            ids
        };
        let mut routes = Vec::new();
        for id in ids {
            if let Some(route) = Self::get(conn, &ModelRouteId::parse(id)?)? {
                routes.push(route);
            }
        }
        Ok(routes)
    }
}

impl AgentModelRouteBindingRepository {
    pub fn insert(conn: &Connection, binding: &AgentModelRouteBinding) -> VibexResult<()> {
        if binding.is_default {
            clear_default(conn, &binding.agent_id, None)?;
        }
        conn.execute(
            "
            INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms, deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                binding.id.as_str(),
                binding.agent_id.as_str(),
                binding.model_route_id.as_str(),
                i64::from(binding.is_default),
                binding.revision.max(1),
                binding.created_at_ms.max(1),
                binding.updated_at_ms.max(1),
                binding.deleted_at_ms,
            ],
        )
        .map_err(storage_err(
            "agent_model_route_binding_create_failed",
            "failed to bind the route to the Agent",
        ))?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        id: &AgentModelRouteBindingId,
    ) -> VibexResult<Option<AgentModelRouteBinding>> {
        conn.query_row(
            "
            SELECT agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms, deleted_at_ms
            FROM agent_model_route_bindings
            WHERE agent_model_route_binding_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![id.as_str()],
            map_binding,
        )
        .optional()
        .map_err(storage_err(
            "agent_model_route_binding_lookup_failed",
            "failed to load the Agent route binding",
        ))
    }

    /// Every live route this Agent may use, default first.
    pub fn list_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_model_route_binding_id, agent_id, model_route_id, is_default,
                    revision, created_at_ms, updated_at_ms, deleted_at_ms
                FROM agent_model_route_bindings
                WHERE agent_id = ?1 AND deleted_at_ms IS NULL
                ORDER BY is_default DESC, created_at_ms ASC, model_route_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the Agent's route bindings",
            ))?;
        let rows = stmt
            .query_map(params![agent_id.as_str()], map_binding)
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the Agent's route bindings",
            ))?;
        collect_rows(rows, "agent_model_route_binding_list_failed")
    }

    /// The Agents that reference this route, for the "used by" line.
    pub fn list_for_route(
        conn: &Connection,
        route_id: &ModelRouteId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_model_route_binding_id, agent_id, model_route_id, is_default,
                    revision, created_at_ms, updated_at_ms, deleted_at_ms
                FROM agent_model_route_bindings
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ORDER BY agent_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the route's Agent bindings",
            ))?;
        let rows = stmt
            .query_map(params![route_id.as_str()], map_binding)
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the route's Agent bindings",
            ))?;
        collect_rows(rows, "agent_model_route_binding_list_failed")
    }

    /// Makes one route the Agent's default, demoting whichever held it.
    pub fn set_default(
        conn: &Connection,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<()> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "agent_model_route_default_transaction_failed",
                "failed to start the route default transaction",
            ))?;
        clear_default(&transaction, agent_id, Some(route_id))?;
        let now = unix_timestamp_ms();
        let changed = transaction
            .execute(
                "
                UPDATE agent_model_route_bindings
                SET is_default = 1,
                    revision = revision + 1,
                    updated_at_ms = ?3
                WHERE agent_id = ?1 AND model_route_id = ?2 AND deleted_at_ms IS NULL
                ",
                params![agent_id.as_str(), route_id.as_str(), now],
            )
            .map_err(storage_err(
                "agent_model_route_default_failed",
                "failed to set the Agent's default route",
            ))?;
        if changed == 0 {
            return Err(VibexError::validation(
                "agent_model_route_binding_not_found",
                "the Agent is not bound to that route",
            )
            .with_diagnostic("agentId", agent_id.as_str())
            .with_diagnostic("modelRouteId", route_id.as_str()));
        }
        transaction.commit().map_err(storage_err(
            "agent_model_route_default_commit_failed",
            "failed to commit the route default",
        ))
    }

    /// Removes one binding. A removed default leaves the Agent with none, which
    /// the caller reports rather than silently promoting another route.
    pub fn delete(
        conn: &Connection,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<bool> {
        let now = unix_timestamp_ms();
        let changed = conn
            .execute(
                "
                UPDATE agent_model_route_bindings
                SET deleted_at_ms = ?3,
                    is_default = 0,
                    revision = revision + 1,
                    updated_at_ms = ?3
                WHERE agent_id = ?1 AND model_route_id = ?2 AND deleted_at_ms IS NULL
                ",
                params![agent_id.as_str(), route_id.as_str(), now],
            )
            .map_err(storage_err(
                "agent_model_route_binding_delete_failed",
                "failed to unbind the route from the Agent",
            ))?;
        Ok(changed > 0)
    }
}

fn clear_default(
    conn: &Connection,
    agent_id: &AgentId,
    except: Option<&ModelRouteId>,
) -> VibexResult<()> {
    let now = unix_timestamp_ms();
    match except {
        Some(route_id) => conn.execute(
            "
            UPDATE agent_model_route_bindings
            SET is_default = 0,
                revision = revision + 1,
                updated_at_ms = ?3
            WHERE agent_id = ?1 AND is_default = 1 AND deleted_at_ms IS NULL
                AND model_route_id != ?2
            ",
            params![agent_id.as_str(), route_id.as_str(), now],
        ),
        None => conn.execute(
            "
            UPDATE agent_model_route_bindings
            SET is_default = 0,
                revision = revision + 1,
                updated_at_ms = ?2
            WHERE agent_id = ?1 AND is_default = 1 AND deleted_at_ms IS NULL
            ",
            params![agent_id.as_str(), now],
        ),
    }
    .map_err(storage_err(
        "agent_model_route_default_clear_failed",
        "failed to clear the Agent's previous default route",
    ))?;
    Ok(())
}

/// The wall-clock stamp a rewrite gets.
///
/// It must move forward, because the launch contract hashes it: a no-op update
/// that kept the old stamp could leave two different route shapes sharing one
/// revision.
fn next_stamp(conn: &Connection, route: &ModelRoute) -> VibexResult<i64> {
    let previous: i64 = conn
        .query_row(
            "SELECT updated_at_ms FROM model_routes WHERE model_route_id = ?1",
            params![route.id.as_str()],
            |row| row.get(0),
        )
        .map_err(storage_err(
            "model_route_stamp_failed",
            "failed to read the route's update stamp",
        ))?;
    Ok(unix_timestamp_ms().max(previous.saturating_add(1)))
}

fn insert_route(conn: &Connection, route: &ModelRoute) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO model_routes (
            model_route_id, display_name, description, kind, strategy,
            first_token_timeout_ms, context_tokens, status, revision,
            created_at_ms, updated_at_ms, deleted_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ",
        params![
            route.id.as_str(),
            route.display_name.trim(),
            route.description.as_deref().map(str::trim),
            enum_to_db(&route.kind)?,
            enum_to_db(&route.strategy)?,
            route.first_token_timeout_ms,
            route.context_tokens,
            enum_to_db(&route.status)?,
            route.revision.max(1),
            route.created_at_ms.max(1),
            route.updated_at_ms.max(1),
            route.deleted_at_ms,
        ],
    )
    .map_err(storage_err(
        "model_route_create_failed",
        "failed to create model route",
    ))?;
    Ok(())
}

fn insert_members(conn: &Connection, route: &ModelRoute) -> VibexResult<()> {
    for member in &route.members {
        conn.execute(
            "
            INSERT INTO model_route_members (
                model_route_member_id, model_route_id, model_provider_profile_id,
                model_id, enabled, order_index, reasoning_effort
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                member.id.as_str(),
                route.id.as_str(),
                member.model_provider_profile_id.as_str(),
                member.model_id.trim(),
                i64::from(member.enabled),
                member.order_index,
                member.reasoning_effort.as_deref().map(str::trim),
            ],
        )
        .map_err(storage_err(
            "model_route_member_create_failed",
            "failed to create a model route member",
        ))?;
    }
    Ok(())
}

fn list_members(conn: &Connection, route_id: &ModelRouteId) -> VibexResult<Vec<ModelRouteMember>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT model_route_member_id, model_provider_profile_id, model_id,
                enabled, order_index, reasoning_effort
            FROM model_route_members
            WHERE model_route_id = ?1
            ORDER BY order_index ASC, model_route_member_id ASC
            ",
        )
        .map_err(storage_err(
            "model_route_members_list_failed",
            "failed to list model route members",
        ))?;
    let rows = stmt
        .query_map(params![route_id.as_str()], |row| {
            Ok(ModelRouteMember {
                id: parse_id_sql(row.get(0)?, ModelRouteMemberId::parse)?,
                model_provider_profile_id: parse_id_sql(
                    row.get(1)?,
                    ModelProviderProfileId::parse,
                )?,
                model_id: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                order_index: row.get(4)?,
                reasoning_effort: row.get(5)?,
            })
        })
        .map_err(storage_err(
            "model_route_members_list_failed",
            "failed to list model route members",
        ))?;
    collect_rows(rows, "model_route_members_list_failed")
}

fn list_where(conn: &Connection, predicate: &str, order: &str) -> VibexResult<Vec<ModelRoute>> {
    let sql = format!(
        "
        SELECT model_route_id, display_name, description, kind, strategy,
            first_token_timeout_ms, context_tokens, status, revision,
            created_at_ms, updated_at_ms, deleted_at_ms
        FROM model_routes
        WHERE {predicate}
        {order}
        "
    );
    let mut stmt = conn.prepare(&sql).map_err(storage_err(
        "model_route_list_failed",
        "failed to list model routes",
    ))?;
    let rows = stmt.query_map([], map_route).map_err(storage_err(
        "model_route_list_failed",
        "failed to list model routes",
    ))?;
    let mut routes = collect_rows(rows, "model_route_list_failed")?;
    for route in &mut routes {
        route.members = list_members(conn, &route.id)?;
    }
    Ok(routes)
}

fn map_route(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelRoute> {
    let kind: RouteKind = enum_from_db_sql(row.get(3)?)?;
    let strategy: RouteStrategy = enum_from_db_sql(row.get(4)?)?;
    let status: RouteStatus = enum_from_db_sql(row.get(7)?)?;
    Ok(ModelRoute {
        id: parse_id_sql(row.get(0)?, ModelRouteId::parse)?,
        display_name: row.get(1)?,
        description: row.get(2)?,
        kind,
        members: Vec::new(),
        strategy,
        first_token_timeout_ms: row
            .get::<_, Option<i64>>(5)?
            .and_then(|value| u64::try_from(value).ok().filter(|value| *value > 0)),
        context_tokens: row
            .get::<_, Option<i64>>(6)?
            .and_then(|value| u32::try_from(value).ok().filter(|value| *value > 0)),
        status,
        revision: row.get(8)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
        deleted_at_ms: row.get(11)?,
    })
}

fn map_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentModelRouteBinding> {
    Ok(AgentModelRouteBinding {
        id: parse_id_sql(row.get(0)?, AgentModelRouteBindingId::parse)?,
        agent_id: parse_id_sql(row.get(1)?, AgentId::parse)?,
        model_route_id: parse_id_sql(row.get(2)?, ModelRouteId::parse)?,
        is_default: row.get::<_, i64>(3)? != 0,
        revision: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        deleted_at_ms: row.get(7)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    code: &'static str,
) -> VibexResult<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(storage_err(code, "failed to read a route row"))?);
    }
    Ok(out)
}

/// Seeds one route from the gateway's current upstream list.
///
/// Before routes existed the gateway had a single global upstream list and a
/// single policy, shared by every Agent pointed at it. This turns that pair into
/// one gateway route and binds it to each of those Agents, so an installation
/// keeps exactly the behaviour it had: same models, same order, same strategy.
///
/// It is idempotent and self-disabling. A database with no upstreams, or one
/// that already holds a route, is left alone — the second condition is what
/// stops a deliberate deletion from resurrecting the route on the next launch.
pub fn backfill_first_route(conn: &Connection) -> VibexResult<Option<ModelRouteId>> {
    let settings = crate::ModelGatewaySettingsRepository::get(conn)?;
    let Some(settings) = settings else {
        return Ok(None);
    };
    if settings.upstreams.is_empty() {
        return Ok(None);
    }
    // Any row at all — live or deleted — means this installation has already
    // been through the migration. Counting only live routes would re-create
    // the seed on the next launch after a user deliberately deleted it.
    let seen: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_routes", [], |row| row.get(0))
        .map_err(storage_err(
            "model_route_seed_probe_failed",
            "failed to check whether routes were already seeded",
        ))?;
    if seen > 0 {
        return Ok(None);
    }
    let profiles = super::ModelProviderProfileRepository::list(conn)?;
    let mut members = Vec::new();
    for upstream_id in &settings.upstreams {
        let Some(profile) = profiles.iter().find(|profile| &profile.id == upstream_id) else {
            // An upstream that no longer exists is dropped rather than failing
            // the whole migration: the route is a best-effort record of what
            // the gateway was actually serving.
            continue;
        };
        for model in profile
            .configured_models
            .iter()
            .filter(|model| model.enabled)
            .take(vibex_core::MAX_ROUTE_MEMBERS.saturating_sub(members.len()))
        {
            members.push(ModelRouteMember::new(
                profile.id.clone(),
                model.id.clone(),
                members.len() as i64,
            ));
            if members.len() >= vibex_core::MAX_ROUTE_MEMBERS {
                break;
            }
        }
    }
    if members.is_empty() {
        return Ok(None);
    }

    let now = unix_timestamp_ms().max(1);
    let route = ModelRoute {
        display_name: settings.display_name.clone(),
        description: Some("Created from the gateway's upstream list".to_string()),
        kind: RouteKind::Gateway,
        members,
        // The gateway's own strategy is the closest existing intent. The route
        // always uses ordered members, because the global setting only ever
        // described how Keys within one Provider were chosen.
        strategy: RouteStrategy::Ordered,
        status: if settings.policy.enabled {
            RouteStatus::Enabled
        } else {
            RouteStatus::Disabled
        },
        revision: 1,
        created_at_ms: now,
        updated_at_ms: now,
        ..Default::default()
    };
    route.validate()?;
    super::ModelRouteRepository::insert(conn, &route)?;

    for agent_id in crate::ModelGatewayAgentRepository::list(conn)? {
        let mut binding = AgentModelRouteBinding::new(agent_id.clone(), route.id.clone());
        binding.is_default = true;
        binding.created_at_ms = now;
        binding.updated_at_ms = now;
        super::AgentModelRouteBindingRepository::insert(conn, &binding)?;
    }
    Ok(Some(route.id))
}

/// Seeds one `Direct` route per legacy direct Provider Profile.
///
/// A direct Profile carries its own `base_url`, its own Key and its own model
/// catalogue, and is projected into exactly one Agent. That is the same shape as
/// a single-member direct route, so it can be expressed as one — which is what
/// lets the route graph show the direct connections beside the gateway ones
/// instead of drawing a second kind of thing.
///
/// The legacy row is *not* removed. The launch path still resolves a Key and a
/// protocol through it, so the route is a description of it, not a replacement
/// for it until the projection moves over.
///
/// Skipped Profiles:
/// - the hidden gateway Profile, which is machinery rather than a user choice;
/// - internal runtime Profiles, which are launch scaffolding with no catalogue;
/// - the per-kind local default, which exists only to give an Agent a fallback.
///
/// Idempotent: a Profile that already has a route is left alone, so this can run
/// on every launch.
pub fn backfill_direct_routes(conn: &Connection) -> VibexResult<usize> {
    let profiles = super::ProviderProfileRepository::list_all(conn)?;
    let mut created = 0usize;
    for profile in profiles {
        if !is_direct_route_candidate(&profile) {
            continue;
        }
        // One route per Profile. The marker lives in the description because it
        // has to survive a rename: the route's own name is the user's to change.
        let marker = direct_route_marker(&profile);
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_routes WHERE description = ?1",
                params![marker],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if exists > 0 {
            continue;
        }
        let Some(route) = direct_route_for(&profile) else {
            continue;
        };
        // The member names the shared Provider the compatibility layer derives
        // from this Profile. A Profile written after the last backfill has no
        // such row yet, so materialise it before the member's foreign key looks
        // for it. This is the same mapping the launch path uses.
        super::ProviderProjectionCompatibilityRepository::sync_legacy_profile(conn, &profile)?;
        super::ModelRouteRepository::insert(conn, &route)?;
        created += 1;
    }
    Ok(created)
}

/// The shared Provider a legacy Profile projects onto.
///
/// The compatibility layer derives the v2 id deterministically, so the same
/// Profile always names the same Provider.
fn legacy_provider_key(profile: &vibex_core::ProviderProfile) -> String {
    format!("model_provider_legacy_{}", profile.id.as_str())
}

/// A Profile that is a real, single-Agent, user-visible direct connection.
///
/// The exclusions mirror the Config Center's own list filter: anything that is
/// machinery must not turn into a user-facing route.
fn is_direct_route_candidate(profile: &vibex_core::ProviderProfile) -> bool {
    if profile.deleted_at_ms.is_some() {
        return false;
    }
    if profile
        .base_url
        .as_deref()
        .is_none_or(|url| url.trim().is_empty())
    {
        // No address means nothing to connect to: an ACP runtime Profile has a
        // command, not an endpoint.
        return false;
    }
    // The hidden gateway Profile, and the internal runtime Profiles, carry a
    // role marker. Either one would be a route the user never made.
    let internal_role = profile
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key.trim() == "vibex.internal.profileRole")
        .map(|entry| entry.value.trim().to_string());
    if internal_role.is_some() {
        return false;
    }
    // The per-kind local default exists so an Agent always has a fallback; it
    // is disabled scaffolding, not a connection the user configured.
    !profile.id.as_str().starts_with("provider_local_")
}

/// The provenance marker a seeded direct route carries.
///
/// It names the legacy Profile and is what makes the backfill idempotent. A
/// display name cannot serve that purpose: renaming a route is a normal edit.
fn direct_route_marker(profile: &vibex_core::ProviderProfile) -> String {
    format!("vibex.direct_provider:{}", profile.id.as_str())
}

fn direct_route_for(profile: &vibex_core::ProviderProfile) -> Option<ModelRoute> {
    let model = profile
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| {
            profile
                .configured_models
                .iter()
                .find(|model| model.enabled)
                .map(|model| model.id.clone())
        })?;
    let provider = ModelProviderProfileId::parse(legacy_provider_key(profile)).ok()?;
    let now = unix_timestamp_ms().max(1);
    Some(ModelRoute {
        display_name: profile.display_name.clone(),
        description: Some(direct_route_marker(profile)),
        kind: RouteKind::Direct,
        members: vec![ModelRouteMember::new(provider, model, 0)],
        strategy: RouteStrategy::Ordered,
        status: RouteStatus::Enabled,
        revision: 1,
        created_at_ms: now,
        updated_at_ms: now,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::ModelProviderProfileStatus;

    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::apply_migrations(&mut connection).unwrap();
        connection
    }

    /// A Provider row to point members at. Members carry a foreign key, so a
    /// route cannot name a Provider that does not exist.
    fn seed_provider(conn: &Connection, name: &str) -> ModelProviderProfileId {
        let profile = vibex_core::ModelProviderProfile {
            display_name: name.to_string(),
            status: ModelProviderProfileStatus::Enabled,
            ..Default::default()
        };
        crate::ModelProviderProfileRepository::insert(conn, &profile).unwrap();
        profile.id
    }

    fn route(provider: &ModelProviderProfileId, models: &[&str]) -> ModelRoute {
        ModelRoute {
            display_name: "Standard".to_string(),
            status: RouteStatus::Enabled,
            members: models
                .iter()
                .enumerate()
                .map(|(index, model)| ModelRouteMember::new(provider.clone(), *model, index as i64))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_route_tables_exist_and_re_applying_migrations_changes_nothing() {
        let mut conn = migrated_connection();
        assert_eq!(crate::current_schema_version(&conn).unwrap(), 67);
        assert!(crate::migration_applied(&conn, 67).unwrap());

        let applied = crate::apply_migrations(&mut conn).unwrap();
        assert!(
            !applied.iter().any(|entry| entry.starts_with("67:")),
            "re-applying migrations must not re-run the route migration"
        );
        // The tables are queryable after a second pass, which is what an
        // existing installation does on every launch.
        assert!(ModelRouteRepository::list(&conn).unwrap().is_empty());
        let agent = AgentId::parse("codex").unwrap();
        assert!(
            AgentModelRouteBindingRepository::list_for_agent(&conn, &agent)
                .unwrap()
                .is_empty()
        );
    }

    /// Puts the gateway row in the shape the pre-route build left it in.
    fn seed_gateway(
        conn: &Connection,
        name: &str,
        upstreams: &[ModelProviderProfileId],
        enabled: bool,
        agents: &[&str],
    ) {
        let settings = crate::ModelGatewaySettingsRepository::get_or_create(conn).unwrap();
        let mut next = settings.clone();
        next.revision = settings.revision + 1;
        next.display_name = name.to_string();
        next.upstreams = upstreams.to_vec();
        next.policy.enabled = enabled;
        crate::ModelGatewaySettingsRepository::update(conn, &next, settings.revision).unwrap();
        for agent in agents {
            crate::ModelGatewayAgentRepository::set(conn, &AgentId::parse(*agent).unwrap(), true)
                .unwrap();
        }
    }

    /// A legacy direct Profile in the shape the Config Center writes: an
    /// address, a model, and one Agent.
    fn seed_direct_profile(
        conn: &Connection,
        agent: &str,
        name: &str,
        base_url: &str,
        model: &str,
        role: Option<&str>,
    ) -> vibex_core::ProviderProfile {
        let options = role.map(|role| vibex_core::ProviderOptions {
            schema_version: 1,
            entries: vec![vibex_core::ProviderBindingMetadata {
                key: "vibex.internal.profileRole".to_string(),
                value: role.to_string(),
            }],
        });
        let profile = crate::ProviderProfileRepository::from_create_request(
            vibex_core::ProviderProfileCreateRequest {
                agent_id: Some(AgentId::parse(agent).unwrap()),
                kind: vibex_core::ProviderKind::Acp,
                display_name: name.to_string(),
                account_alias: None,
                base_url: Some(base_url.to_string()),
                default_model: Some(model.to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: options,
                secret_references: Vec::new(),
            },
        );
        crate::ProviderProfileRepository::insert(conn, &profile).unwrap();
        profile
    }

    #[test]
    fn a_direct_profile_becomes_a_single_member_direct_route() {
        let conn = migrated_connection();
        seed_direct_profile(
            &conn,
            "codex",
            "DeepSeek 直连",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );

        let created = backfill_direct_routes(&conn).unwrap();
        assert_eq!(created, 1);

        let routes = ModelRouteRepository::list(&conn).unwrap();
        assert_eq!(routes.len(), 1);
        let route = &routes[0];
        assert_eq!(route.kind, RouteKind::Direct);
        assert_eq!(route.display_name, "DeepSeek 直连");
        assert_eq!(
            route.members.len(),
            1,
            "a direct route has exactly one member"
        );
        assert_eq!(route.members[0].model_id, "deepseek-chat");
    }

    #[test]
    fn a_gateway_profile_is_not_turned_into_a_direct_route() {
        let conn = migrated_connection();
        // The hidden Profile the gateway projects into an Agent carries the
        // gateway role. It is machinery, so it must not become a route the user
        // never made.
        seed_direct_profile(
            &conn,
            "codex",
            "Vibex Gateway",
            "http://127.0.0.1:3425",
            "deepseek/deepseek-chat",
            Some("model_gateway"),
        );
        // An internal runtime Profile has a role too, and no catalogue at all.
        seed_direct_profile(
            &conn,
            "claude",
            "Claude ACP",
            "https://api.anthropic.com",
            "claude-sonnet-5",
            Some("agent_runtime"),
        );

        assert_eq!(backfill_direct_routes(&conn).unwrap(), 0);
        assert!(ModelRouteRepository::list(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_profile_without_an_address_or_a_model_is_skipped() {
        let conn = migrated_connection();
        // No base_url: an ACP runtime Profile has a command, not an endpoint.
        seed_direct_profile(&conn, "codex", "No address", "", "m", None);
        assert_eq!(backfill_direct_routes(&conn).unwrap(), 0);
    }

    #[test]
    fn the_direct_backfill_is_idempotent_across_launches() {
        let conn = migrated_connection();
        seed_direct_profile(
            &conn,
            "codex",
            "DeepSeek 直连",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );

        assert_eq!(backfill_direct_routes(&conn).unwrap(), 1);
        assert_eq!(
            backfill_direct_routes(&conn).unwrap(),
            0,
            "a re-run seeds nothing"
        );
        assert_eq!(ModelRouteRepository::list(&conn).unwrap().len(), 1);

        // Renaming the route must not make the backfill create it again: the
        // marker is provenance, not the display name.
        let mut route = ModelRouteRepository::list(&conn).unwrap().remove(0);
        route.display_name = "Renamed".to_string();
        route.revision += 1;
        ModelRouteRepository::update(&conn, &route, route.revision - 1).unwrap();
        assert_eq!(backfill_direct_routes(&conn).unwrap(), 0);
        assert_eq!(ModelRouteRepository::list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn two_agents_keep_two_independent_direct_routes() {
        let conn = migrated_connection();
        seed_direct_profile(
            &conn,
            "codex",
            "DeepSeek",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );
        seed_direct_profile(
            &conn,
            "claude",
            "DeepSeek",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );

        // The same Provider for two Agents is two Profiles, so two routes. They
        // must stay separate: an edit to one Agent's protocol is not an edit to
        // the other's.
        assert_eq!(backfill_direct_routes(&conn).unwrap(), 2);
        let routes = ModelRouteRepository::list(&conn).unwrap();
        assert_eq!(routes.len(), 2);
        assert!(routes.iter().all(|route| route.kind == RouteKind::Direct));
    }

    #[test]
    fn the_first_route_is_built_from_the_gateway_upstreams() {
        let conn = migrated_connection();
        let deepseek = seed_provider(&conn, "DeepSeek");
        let kimi = seed_provider(&conn, "Kimi");
        // Give each Provider a catalogue so the backfill has models to copy.
        for (id, model) in [(&deepseek, "deepseek-chat"), (&kimi, "kimi-k3")] {
            let mut profile = crate::ModelProviderProfileRepository::get(&conn, id)
                .unwrap()
                .unwrap();
            profile.configured_models = vec![vibex_core::ModelProviderCatalogEntry {
                id: model.to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            }];
            profile.revision += 1;
            crate::ModelProviderProfileRepository::update(&conn, &profile, profile.revision - 1)
                .unwrap();
        }
        seed_gateway(
            &conn,
            "Vibex Gateway",
            &[deepseek.clone(), kimi.clone()],
            true,
            &["codex", "claude"],
        );

        let created = backfill_first_route(&conn)
            .unwrap()
            .expect("a route is seeded");
        let route = ModelRouteRepository::get(&conn, &created).unwrap().unwrap();
        assert_eq!(route.display_name, "Vibex Gateway");
        assert_eq!(route.kind, RouteKind::Gateway);
        assert_eq!(route.status, RouteStatus::Enabled);
        assert_eq!(
            route
                .members
                .iter()
                .map(|member| member.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["deepseek-chat", "kimi-k3"],
            "upstream order decides member order"
        );

        // Every Agent that was on the gateway now has exactly that route, as
        // its default, so nothing changes for it.
        for agent_id in ["codex", "claude"] {
            let agent = AgentId::parse(agent_id).unwrap();
            let bindings = AgentModelRouteBindingRepository::list_for_agent(&conn, &agent).unwrap();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].model_route_id, route.id);
            assert!(bindings[0].is_default);
        }
    }

    #[test]
    fn the_backfill_is_idempotent_and_does_not_resurrect_a_deleted_route() {
        let conn = migrated_connection();
        let deepseek = seed_provider(&conn, "DeepSeek");
        let mut profile = crate::ModelProviderProfileRepository::get(&conn, &deepseek)
            .unwrap()
            .unwrap();
        profile.configured_models = vec![vibex_core::ModelProviderCatalogEntry {
            id: "m".to_string(),
            display_name: None,
            enabled: true,
            metadata: Vec::new(),
            capabilities: Default::default(),
        }];
        profile.revision += 1;
        crate::ModelProviderProfileRepository::update(&conn, &profile, profile.revision - 1)
            .unwrap();
        seed_gateway(&conn, "Vibex Gateway", &[deepseek], true, &["codex"]);

        let first = backfill_first_route(&conn).unwrap().expect("seeded once");
        assert!(
            backfill_first_route(&conn).unwrap().is_none(),
            "a second run must not add a second route"
        );
        assert_eq!(ModelRouteRepository::list(&conn).unwrap().len(), 1);

        // Deleting the seeded route must stick: a later launch finds no routes
        // but the gateway still lists upstreams, and must not re-create it.
        ModelRouteRepository::delete(&conn, &first).unwrap();
        assert!(backfill_first_route(&conn).unwrap().is_none());
        assert!(ModelRouteRepository::list(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_gateway_with_no_upstreams_seeds_nothing() {
        let conn = migrated_connection();
        seed_gateway(&conn, "Vibex Gateway", &[], true, &["codex"]);
        assert!(backfill_first_route(&conn).unwrap().is_none());
        assert!(ModelRouteRepository::list(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_disabled_gateway_seeds_a_disabled_route() {
        let conn = migrated_connection();
        let deepseek = seed_provider(&conn, "DeepSeek");
        let mut profile = crate::ModelProviderProfileRepository::get(&conn, &deepseek)
            .unwrap()
            .unwrap();
        profile.configured_models = vec![vibex_core::ModelProviderCatalogEntry {
            id: "m".to_string(),
            display_name: None,
            enabled: true,
            metadata: Vec::new(),
            capabilities: Default::default(),
        }];
        profile.revision += 1;
        crate::ModelProviderProfileRepository::update(&conn, &profile, profile.revision - 1)
            .unwrap();
        seed_gateway(&conn, "Vibex Gateway", &[deepseek], false, &["codex"]);

        let created = backfill_first_route(&conn).unwrap().expect("still seeded");
        let route = ModelRouteRepository::get(&conn, &created).unwrap().unwrap();
        assert_eq!(
            route.status,
            RouteStatus::Disabled,
            "a turned-off gateway must not produce an enabled route"
        );
    }

    #[test]
    fn a_route_round_trips_with_its_members_in_attempt_order() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let mut route = route(&provider, &["first", "second", "third"]);
        route.description = Some("primary and fallback".to_string());
        route.strategy = RouteStrategy::LeastUsed;
        ModelRouteRepository::insert(&conn, &route).unwrap();

        let loaded = ModelRouteRepository::get(&conn, &route.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.display_name, "Standard");
        assert_eq!(loaded.strategy, RouteStrategy::LeastUsed);
        assert_eq!(loaded.kind, RouteKind::Gateway);
        assert_eq!(
            loaded
                .members
                .iter()
                .map(|member| member.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
    }

    #[test]
    fn updating_a_route_replaces_the_member_order_wholesale() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let mut route = route(&provider, &["first", "second"]);
        ModelRouteRepository::insert(&conn, &route).unwrap();

        // Reverse the order without changing which members exist.
        route.members.reverse();
        for (index, member) in route.members.iter_mut().enumerate() {
            member.order_index = index as i64;
        }
        let updated = ModelRouteRepository::update(&conn, &route, route.revision).unwrap();
        assert_eq!(updated.revision, route.revision + 1);
        assert_eq!(
            updated
                .members
                .iter()
                .map(|member| member.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
    }

    #[test]
    fn a_stale_revision_is_refused_rather_than_overwriting() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let route = route(&provider, &["first"]);
        ModelRouteRepository::insert(&conn, &route).unwrap();

        let error = ModelRouteRepository::update(&conn, &route, route.revision + 5).unwrap_err();
        assert_eq!(error.code, "model_route_revision_conflict");
    }

    #[test]
    fn a_direct_route_cannot_be_saved_with_two_members() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let mut route = route(&provider, &["first", "second"]);
        route.kind = RouteKind::Direct;
        let error = ModelRouteRepository::insert(&conn, &route).unwrap_err();
        assert_eq!(error.code, "route_direct_member_count_invalid");
    }

    #[test]
    fn one_agent_may_bind_several_routes_but_only_one_is_default() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let agent = AgentId::parse("codex").unwrap();
        let mut first = route(&provider, &["a"]);
        first.display_name = "First".to_string();
        let mut second = route(&provider, &["b"]);
        second.display_name = "Second".to_string();
        ModelRouteRepository::insert(&conn, &first).unwrap();
        ModelRouteRepository::insert(&conn, &second).unwrap();

        let mut first_binding = AgentModelRouteBinding::new(agent.clone(), first.id.clone());
        first_binding.is_default = true;
        AgentModelRouteBindingRepository::insert(&conn, &first_binding).unwrap();

        let mut second_binding = AgentModelRouteBinding::new(agent.clone(), second.id.clone());
        second_binding.is_default = true;
        AgentModelRouteBindingRepository::insert(&conn, &second_binding).unwrap();

        let bindings = AgentModelRouteBindingRepository::list_for_agent(&conn, &agent).unwrap();
        assert_eq!(bindings.len(), 2, "both routes stay bound");
        assert_eq!(
            bindings
                .iter()
                .filter(|binding| binding.is_default)
                .map(|binding| binding.model_route_id.clone())
                .collect::<Vec<_>>(),
            vec![second.id.clone()],
            "binding a second default demotes the first"
        );
        // Default first, so a new session finds it without searching.
        assert_eq!(bindings[0].model_route_id, second.id);
    }

    #[test]
    fn the_database_refuses_a_second_default_for_one_agent() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let agent = AgentId::parse("codex").unwrap();
        let first = route(&provider, &["a"]);
        let second = route(&provider, &["b"]);
        ModelRouteRepository::insert(&conn, &first).unwrap();
        ModelRouteRepository::insert(&conn, &second).unwrap();

        // The write path would demote the first. Writing the rows directly
        // skips that, which is exactly what the partial index must catch.
        conn.execute(
            "INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 1, 1, 1, 1)",
            params![
                AgentModelRouteBindingId::new().as_str(),
                agent.as_str(),
                first.id.as_str()
            ],
        )
        .unwrap();

        let duplicate = conn.execute(
            "INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 1, 1, 1, 1)",
            params![
                AgentModelRouteBindingId::new().as_str(),
                agent.as_str(),
                second.id.as_str()
            ],
        );
        assert!(
            duplicate.is_err(),
            "the partial unique index is the last fence against two defaults"
        );

        // A non-default binding for the same Agent is still allowed.
        conn.execute(
            "INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 0, 1, 1, 1)",
            params![
                AgentModelRouteBindingId::new().as_str(),
                agent.as_str(),
                second.id.as_str()
            ],
        )
        .unwrap();
    }

    #[test]
    fn set_default_promotes_one_and_demotes_the_other() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let agent = AgentId::parse("claude").unwrap();
        let first = route(&provider, &["a"]);
        let second = route(&provider, &["b"]);
        ModelRouteRepository::insert(&conn, &first).unwrap();
        ModelRouteRepository::insert(&conn, &second).unwrap();

        let mut binding = AgentModelRouteBinding::new(agent.clone(), first.id.clone());
        binding.is_default = true;
        AgentModelRouteBindingRepository::insert(&conn, &binding).unwrap();
        AgentModelRouteBindingRepository::insert(
            &conn,
            &AgentModelRouteBinding::new(agent.clone(), second.id.clone()),
        )
        .unwrap();

        AgentModelRouteBindingRepository::set_default(&conn, &agent, &second.id).unwrap();
        let bindings = AgentModelRouteBindingRepository::list_for_agent(&conn, &agent).unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].model_route_id, second.id);
        assert!(bindings[0].is_default);
        assert!(!bindings[1].is_default);
    }

    #[test]
    fn deleting_a_route_unbinds_it_from_every_agent() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let shared = route(&provider, &["a"]);
        ModelRouteRepository::insert(&conn, &shared).unwrap();
        for agent_id in ["codex", "claude"] {
            let agent = AgentId::parse(agent_id).unwrap();
            let mut binding = AgentModelRouteBinding::new(agent.clone(), shared.id.clone());
            binding.is_default = true;
            AgentModelRouteBindingRepository::insert(&conn, &binding).unwrap();
        }

        assert!(ModelRouteRepository::delete(&conn, &shared.id).unwrap());
        assert!(
            ModelRouteRepository::get(&conn, &shared.id)
                .unwrap()
                .is_none()
        );
        for agent_id in ["codex", "claude"] {
            let agent = AgentId::parse(agent_id).unwrap();
            assert!(
                AgentModelRouteBindingRepository::list_for_agent(&conn, &agent)
                    .unwrap()
                    .is_empty(),
                "an unbound route must not leave a live binding behind"
            );
        }
    }

    #[test]
    fn a_route_reports_the_agents_that_reference_it() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let shared = route(&provider, &["a"]);
        ModelRouteRepository::insert(&conn, &shared).unwrap();
        for agent_id in ["codex", "gemini"] {
            let agent = AgentId::parse(agent_id).unwrap();
            AgentModelRouteBindingRepository::insert(
                &conn,
                &AgentModelRouteBinding::new(agent, shared.id.clone()),
            )
            .unwrap();
        }

        let users = AgentModelRouteBindingRepository::list_for_route(&conn, &shared.id).unwrap();
        assert_eq!(
            users
                .iter()
                .map(|binding| binding.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["codex", "gemini"]
        );
    }

    #[test]
    fn list_enabled_skips_a_disabled_route_and_keeps_names_in_order() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let mut beta = route(&provider, &["a"]);
        beta.display_name = "Beta".to_string();
        let mut alpha = route(&provider, &["b"]);
        alpha.display_name = "Alpha".to_string();
        let mut off = route(&provider, &["c"]);
        off.display_name = "Zeta".to_string();
        off.status = RouteStatus::Disabled;
        for route in [&beta, &alpha, &off] {
            ModelRouteRepository::insert(&conn, route).unwrap();
        }

        let enabled = ModelRouteRepository::list_enabled(&conn).unwrap();
        assert_eq!(
            enabled
                .iter()
                .map(|route| route.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Beta"]
        );
    }

    #[test]
    fn a_provider_lookup_lists_every_route_that_names_it() {
        let conn = migrated_connection();
        let deepseek = seed_provider(&conn, "DeepSeek");
        let kimi = seed_provider(&conn, "Kimi");
        let mut naming_both = route(&deepseek, &["a"]);
        naming_both
            .members
            .push(ModelRouteMember::new(kimi.clone(), "k3", 1));
        let only_deepseek = route(&deepseek, &["b"]);
        ModelRouteRepository::insert(&conn, &naming_both).unwrap();
        ModelRouteRepository::insert(&conn, &only_deepseek).unwrap();

        let naming = ModelRouteRepository::list_naming_provider(&conn, &deepseek).unwrap();
        assert_eq!(naming.len(), 2, "both routes name DeepSeek");
        let naming_kimi = ModelRouteRepository::list_naming_provider(&conn, &kimi).unwrap();
        assert_eq!(naming_kimi.len(), 1, "only one names Kimi");
    }
}
