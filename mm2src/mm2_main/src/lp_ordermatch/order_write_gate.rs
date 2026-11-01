//! The storage order of one maker order (CRD ch.12 R7a, R7b).
//!
//! The paths of the order-matching layer that change a maker order keep its own lock for in-memory work only,
//! so the file or database write that follows a change runs with no order lock held. Once the writes run
//! outside the lock, two writers of one order can reach storage in a different order than they made their
//! changes in. This gate restores that order: every storage write of an order passes through the order's gate.
//!
//! * A writer *publishes* the order's state to the gate while it holds the order's lock. The gate keeps only the
//!   newest published state and stamps it with a ticket, so the tickets follow the order of the changes.
//! * The writer then releases the order's lock and *completes* its ticket. Completing takes the gate's I/O lock,
//!   which serializes the writes of the order, and writes the newest published state if no earlier completion has
//!   written it already. A state that a newer one has superseded is never written, so the stored record does not
//!   go backwards.
//! * The removal of the order from the container marks the gate as removed in the same step. A removed gate
//!   accepts no more states and writes nothing but what was published before the removal, so no write recreates the
//!   active record of a removed order. The removal itself passes through the gate: it first writes the state that
//!   is still unwritten, so the history entry it saves carries the order's last state, and then deletes the order.
//!
//! Lock order: a task may hold the order's lock while it waits for the gate (the negotiation paths do), but it
//! never takes the order's lock while it holds the gate's I/O lock, and the gate never takes the order's lock.
//!
//! The gate belongs to the order's entry in [`MakerOrdersContext`]. A removed entry is dropped from there only
//! when no writer holds its gate any more, so a late writer still finds the mark of the removal.
use super::*;
use my_orders_storage::{write_maker_order_state, MyOrdersError, MyOrdersResult};

/// The state of one order that waits to be stored.
#[derive(Clone)]
struct PendingWrite {
    ticket: u64,
    order: MakerOrder,
    /// Whether the history database row is to be updated as well (set by `update_maker_order`, and kept until
    /// written, even when a newer state that does not ask for it supersedes this one).
    with_history: bool,
}

#[derive(Default)]
struct GateState {
    last_ticket: u64,
    pending: Option<PendingWrite>,
    /// The newest ticket that a successful write covered.
    written_upto: u64,
    /// The newest ticket that a failed write covered, and why it failed.
    failed_upto: u64,
    failure: String,
    removed: bool,
}

/// The gate through which every storage write of one maker order passes.
#[derive(Default)]
pub(crate) struct OrderWriteGate {
    /// Short, synchronous, never held across an `await`.
    state: PaMutex<GateState>,
    /// Held while the order is written to storage or deleted from it.
    io: AsyncMutex<()>,
}

impl OrderWriteGate {
    /// Whether the order has been removed from the container.
    pub(crate) fn is_removed(&self) -> bool { self.state.lock().removed }

    /// Records the state of the order as the one to store. Call it with the order's lock held, right after the
    /// change, so that the tickets follow the order of the changes. Returns `None` if the order has been removed:
    /// nothing is to be stored then.
    pub(crate) fn publish(&self, order: &MakerOrder, with_history: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if state.removed {
            return None;
        }
        state.last_ticket += 1;
        let ticket = state.last_ticket;
        let with_history = with_history || state.pending.as_ref().is_some_and(|p| p.with_history);
        state.pending = Some(PendingWrite {
            ticket,
            order: order.clone(),
            with_history,
        });
        Some(ticket)
    }

    /// Marks the order as removed. The container does it in the step in which the order leaves it.
    pub(crate) fn mark_removed(&self) { self.state.lock().removed = true; }

    /// Stores the state that `ticket` was published with, or a newer one, unless a completion has stored it
    /// already. Call it with no order lock held, or with the order's lock held where the caller needs the write
    /// to be done before it goes on. If an earlier write covered the ticket and failed, so does this call.
    pub(crate) async fn complete(&self, ctx: &MmArc, ticket: u64) -> MyOrdersResult<()> {
        let _io = self.io.lock().await;
        let pending = {
            let state = self.state.lock();
            if state.written_upto >= ticket || state.removed {
                // Stored by an earlier completion, or the order is gone and its removal has the last word.
                return Ok(());
            }
            if state.failed_upto >= ticket {
                return MmError::err(MyOrdersError::ErrorSaving(state.failure.clone()));
            }
            match &state.pending {
                Some(pending) => pending.clone(),
                None => return Ok(()),
            }
        };
        self.write(ctx, pending).await
    }

    /// Writes one pending state and records how it went. The caller holds the I/O lock.
    async fn write(&self, ctx: &MmArc, pending: PendingWrite) -> MyOrdersResult<()> {
        let ticket = pending.ticket;
        let result = write_maker_order_state(ctx.clone(), pending.order, pending.with_history)
            .compat()
            .await;
        let mut state = self.state.lock();
        match &result {
            Ok(()) => state.written_upto = ticket,
            Err(e) => {
                state.failed_upto = ticket;
                state.failure = e.to_string();
            },
        }
        // A newer state published during the write stays pending.
        if state.pending.as_ref().is_some_and(|p| p.ticket == ticket) {
            state.pending = None;
        }
        result
    }

    /// Deletes the order from storage after its removal from the container (the gate is marked removed). The
    /// state that is still unwritten is written first, so the history entry that the deletion saves carries the
    /// order's last state, and a write that is still in progress is waited for.
    async fn delete(&self, ctx: &MmArc, order: MakerOrder, reason: MakerOrderCancellationReason) {
        let _io = self.io.lock().await;
        let pending = self.state.lock().pending.clone();
        if let Some(pending) = pending {
            self.write(ctx, pending)
                .await
                .error_log_with_msg("!write_maker_order_state before the removal");
        }
        delete_my_maker_order(ctx.clone(), order, reason).compat().await.ok();
    }
}

/// An order that left the container and is yet to be deleted from storage.
pub(crate) struct RemovedMakerOrder {
    gate: Arc<OrderWriteGate>,
    order: MakerOrder,
}

impl RemovedMakerOrder {
    /// Removes `order` from the container; its gate is marked removed in that step. Call it with the order's lock
    /// held, with `order` the locked state. `None` if another task has removed the order already.
    pub(crate) fn take_out(ordermatch_ctx: &OrdermatchContext, order: &MakerOrder) -> Option<RemovedMakerOrder> {
        let (_, gate) = ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .remove_order_with_gate(&order.uuid)?;
        Some(RemovedMakerOrder {
            gate,
            order: order.clone(),
        })
    }

    /// The removal of an order that the container has dropped already (an expired order; its gate was marked in
    /// the step that dropped it). Call it with the order's lock held, with `order` the locked state.
    pub(crate) fn already_dropped(gate: Arc<OrderWriteGate>, order: &MakerOrder) -> RemovedMakerOrder {
        RemovedMakerOrder {
            gate,
            order: order.clone(),
        }
    }

    /// Deletes the order from storage. Call it with no order lock held.
    pub(crate) async fn delete(self, ctx: &MmArc, reason: MakerOrderCancellationReason) {
        self.gate.delete(ctx, self.order, reason).await
    }
}

/// A state of an order that was published and waits to be stored.
pub(crate) struct PublishedState {
    gate: Arc<OrderWriteGate>,
    ticket: u64,
}

impl PublishedState {
    /// Publishes the state of `order` for storage. Call it with the order's lock held, right after the change.
    /// `None` if the order is not in the container any more: nothing is to be stored then.
    pub(crate) fn publish(
        ordermatch_ctx: &OrdermatchContext,
        order: &MakerOrder,
        with_history: bool,
    ) -> Option<PublishedState> {
        let gate = ordermatch_ctx.maker_orders_ctx.lock().live_write_gate(&order.uuid)?;
        let ticket = gate.publish(order, with_history)?;
        Some(PublishedState { gate, ticket })
    }

    /// Stores the published state (see [`OrderWriteGate::complete`]).
    pub(crate) async fn complete(&self, ctx: &MmArc) -> MyOrdersResult<()> {
        self.gate.complete(ctx, self.ticket).await
    }
}
