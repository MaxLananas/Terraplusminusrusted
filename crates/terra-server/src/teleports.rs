//! Pending `/tpll` teleports waiting for chunk data, polled from the server tick without
//! blocking. One pending teleport per player (a new one replaces and cancels the old), a global
//! bound with refusal when full, and a deadline per request.

use crate::tpll::{Authorization, LinkedWorlds, TeleportWorld, TpllRequest, TpllStep};
use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};
use terra_chunk_generation::{ChunkLoader, ChunkTicket, LoaderError};

#[derive(Debug, Clone, PartialEq)]
pub enum TeleportOutcome {
    Step(TpllStep),
    /// The chunk data could not be loaded in time or failed; upstream says "This location is
    /// currently unavailable. Please try again later."
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleportRefusal {
    TooManyPending,
    LoaderOverloaded,
    LoaderShutDown,
}

struct Pending {
    request: TpllRequest,
    ticket: ChunkTicket,
    local: (usize, usize),
    authorization: Authorization,
    deadline: Instant,
}

pub struct PendingTeleports<P> {
    entries: HashMap<P, Pending>,
    capacity: usize,
    timeout: Duration,
}

impl<P: Eq + Hash + Clone> PendingTeleports<P> {
    pub fn new(capacity: usize, timeout: Duration) -> Self {
        Self { entries: HashMap::new(), capacity: capacity.max(1), timeout }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Queues the surface lookup a [`TpllStep::NeedsSurface`] asked for. Never blocks.
    pub fn submit(&mut self, player: P, request: TpllRequest, step: &TpllStep, authorization: Authorization, loader: &ChunkLoader) -> Result<(), TeleportRefusal> {
        let TpllStep::NeedsSurface { chunk_x, chunk_z, local_x, local_z } = *step else { return Ok(()) };
        if !self.entries.contains_key(&player) && self.entries.len() >= self.capacity {
            return Err(TeleportRefusal::TooManyPending);
        }
        let ticket = loader.request(chunk_x, chunk_z).map_err(|error| match error {
            LoaderError::ShutDown => TeleportRefusal::LoaderShutDown,
            _ => TeleportRefusal::LoaderOverloaded,
        })?;
        self.entries.insert(player, Pending { request, ticket, local: (local_x, local_z), authorization, deadline: Instant::now() + self.timeout });
        Ok(())
    }

    pub fn cancel(&mut self, player: &P) -> bool {
        self.entries.remove(player).is_some()
    }

    /// Resolves every pending teleport whose chunk data is ready, failed or overdue.
    pub fn poll(&mut self, linked: &LinkedWorlds, world: &dyn TeleportWorld) -> Vec<(P, TeleportOutcome)> {
        let now = Instant::now();
        let mut finished = Vec::new();
        for (player, pending) in &mut self.entries {
            let outcome = match pending.ticket.try_get() {
                Some(Ok(terrain)) => {
                    let surface = terrain.surface_height(pending.local.0, pending.local.1);
                    Some(TeleportOutcome::Step(pending.request.resume_with_surface(surface, linked, pending.authorization, world)))
                }
                Some(Err(error)) => Some(TeleportOutcome::Unavailable(error.to_string())),
                None if now >= pending.deadline => Some(TeleportOutcome::Unavailable(LoaderError::TimedOut.to_string())),
                None => None,
            };
            if let Some(outcome) = outcome {
                finished.push((player.clone(), outcome));
            }
        }
        for (player, _) in &finished {
            self.entries.remove(player);
        }
        finished
    }
}
