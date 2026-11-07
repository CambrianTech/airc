use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{
    BranchCheckRollupArgs, GhCheck, GhClient, GhError, IssueView, IssueViewArgs, MergeReceipt,
    PrCreateArgs, PrCreated, PrEditBaseArgs, PrMergeArgs, PrView, PrViewArgs,
};

/// Per-method response queue + per-method call record. All state
/// is `Mutex`-protected for `&self` interior mutability (the
/// trait methods take `&self`).
#[derive(Default)]
pub struct MockGhClient {
    pr_view_queue: Mutex<VecDeque<Result<PrView, GhError>>>,
    pr_create_queue: Mutex<VecDeque<Result<PrCreated, GhError>>>,
    pr_merge_queue: Mutex<VecDeque<Result<MergeReceipt, GhError>>>,
    pr_edit_base_queue: Mutex<VecDeque<Result<(), GhError>>>,
    branch_check_rollup_queue: Mutex<VecDeque<Result<Vec<GhCheck>, GhError>>>,
    issue_view_queue: Mutex<VecDeque<Result<IssueView, GhError>>>,

    pr_view_calls: Mutex<Vec<PrViewArgs>>,
    pr_create_calls: Mutex<Vec<PrCreateArgs>>,
    pr_merge_calls: Mutex<Vec<PrMergeArgs>>,
    pr_edit_base_calls: Mutex<Vec<PrEditBaseArgs>>,
    branch_check_rollup_calls: Mutex<Vec<BranchCheckRollupArgs>>,
    issue_view_calls: Mutex<Vec<IssueViewArgs>>,
}

impl MockGhClient {
    pub fn new() -> Self {
        Self::default()
    }

    // --- queue responses ---

    /// Queue the next [`GhClient::pr_view`] outcome. FIFO.
    pub fn queue_pr_view(&self, result: Result<PrView, GhError>) {
        self.pr_view_queue.lock().unwrap().push_back(result);
    }

    pub fn queue_pr_create(&self, result: Result<PrCreated, GhError>) {
        self.pr_create_queue.lock().unwrap().push_back(result);
    }

    pub fn queue_pr_merge(&self, result: Result<MergeReceipt, GhError>) {
        self.pr_merge_queue.lock().unwrap().push_back(result);
    }

    pub fn queue_pr_edit_base(&self, result: Result<(), GhError>) {
        self.pr_edit_base_queue.lock().unwrap().push_back(result);
    }

    pub fn queue_branch_check_rollup(&self, result: Result<Vec<GhCheck>, GhError>) {
        self.branch_check_rollup_queue
            .lock()
            .unwrap()
            .push_back(result);
    }

    /// Queue the next [`GhClient::issue_view`] outcome. FIFO. Card #356.
    pub fn queue_issue_view(&self, result: Result<IssueView, GhError>) {
        self.issue_view_queue.lock().unwrap().push_back(result);
    }

    // --- call records ---

    pub fn pr_view_call_count(&self) -> usize {
        self.pr_view_calls.lock().unwrap().len()
    }
    pub fn pr_create_call_count(&self) -> usize {
        self.pr_create_calls.lock().unwrap().len()
    }
    pub fn pr_merge_call_count(&self) -> usize {
        self.pr_merge_calls.lock().unwrap().len()
    }
    pub fn pr_edit_base_call_count(&self) -> usize {
        self.pr_edit_base_calls.lock().unwrap().len()
    }
    pub fn branch_check_rollup_call_count(&self) -> usize {
        self.branch_check_rollup_calls.lock().unwrap().len()
    }

    /// Snapshot of every [`GhClient::pr_view`] call's args, in
    /// invocation order. Useful for asserting "the merger
    /// queried PR 42 first, then PR 43" patterns.
    pub fn received_pr_view_calls(&self) -> Vec<PrViewArgs> {
        self.pr_view_calls.lock().unwrap().clone()
    }
    pub fn received_pr_create_calls(&self) -> Vec<PrCreateArgs> {
        self.pr_create_calls.lock().unwrap().clone()
    }
    pub fn received_pr_merge_calls(&self) -> Vec<PrMergeArgs> {
        self.pr_merge_calls.lock().unwrap().clone()
    }
    pub fn received_pr_edit_base_calls(&self) -> Vec<PrEditBaseArgs> {
        self.pr_edit_base_calls.lock().unwrap().clone()
    }
    pub fn received_branch_check_rollup_calls(&self) -> Vec<BranchCheckRollupArgs> {
        self.branch_check_rollup_calls.lock().unwrap().clone()
    }
    pub fn received_issue_view_calls(&self) -> Vec<IssueViewArgs> {
        self.issue_view_calls.lock().unwrap().clone()
    }
}

/// Build the "no response queued" error so test failures name
/// the method that wasn't set up. Centralized so the message
/// shape is consistent.
fn unqueued(method: &str) -> GhError {
    GhError::Process(std::io::Error::other(format!(
        "MockGhClient::{method} called but no response was queued — \
             set one via queue_{method}() before invoking",
    )))
}

#[async_trait]
impl GhClient for MockGhClient {
    async fn pr_view(&self, args: PrViewArgs) -> Result<PrView, GhError> {
        self.pr_view_calls.lock().unwrap().push(args);
        self.pr_view_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(unqueued("pr_view")))
    }

    async fn pr_create(&self, args: PrCreateArgs) -> Result<PrCreated, GhError> {
        self.pr_create_calls.lock().unwrap().push(args);
        self.pr_create_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(unqueued("pr_create")))
    }

    async fn pr_merge(&self, args: PrMergeArgs) -> Result<MergeReceipt, GhError> {
        self.pr_merge_calls.lock().unwrap().push(args);
        self.pr_merge_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(unqueued("pr_merge")))
    }

    async fn pr_edit_base(&self, args: PrEditBaseArgs) -> Result<(), GhError> {
        self.pr_edit_base_calls.lock().unwrap().push(args);
        self.pr_edit_base_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(unqueued("pr_edit_base")))
    }

    async fn branch_check_rollup(
        &self,
        args: BranchCheckRollupArgs,
    ) -> Result<Vec<GhCheck>, GhError> {
        self.branch_check_rollup_calls.lock().unwrap().push(args);
        self.branch_check_rollup_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(unqueued("branch_check_rollup")))
    }

    async fn issue_view(&self, args: IssueViewArgs) -> Result<IssueView, GhError> {
        self.issue_view_calls.lock().unwrap().push(args);
        self.issue_view_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(unqueued("issue_view")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_for_pr(number: u64) -> PrViewArgs {
        PrViewArgs {
            repo: "CambrianTech/airc".to_string(),
            number,
            cwd: None,
        }
    }

    fn ok_view(state: &str) -> PrView {
        PrView {
            state: state.to_string(),
            mergeable: "MERGEABLE".to_string(),
            status_check_rollup: Some(Vec::new()),
            merged_at: None,
            base_ref_name: None,
            base_ref_oid: None,
            head_ref_oid: None,
        }
    }

    #[tokio::test]
    async fn queued_pr_view_responses_fire_in_order() {
        let mock = MockGhClient::new();
        mock.queue_pr_view(Ok(ok_view("OPEN")));
        mock.queue_pr_view(Ok(ok_view("MERGED")));

        let first = mock.pr_view(args_for_pr(42)).await.unwrap();
        let second = mock.pr_view(args_for_pr(43)).await.unwrap();

        assert_eq!(first.state, "OPEN");
        assert_eq!(second.state, "MERGED");
    }

    #[tokio::test]
    async fn unqueued_response_returns_loud_error_with_method_name() {
        let mock = MockGhClient::new();
        let err = mock.pr_view(args_for_pr(1)).await.unwrap_err();
        let GhError::Process(io) = err else {
            panic!("expected GhError::Process for empty queue, got something else");
        };
        assert!(
            io.to_string().contains("MockGhClient::pr_view"),
            "error must name the unset method: {io}"
        );
        assert!(
            io.to_string().contains("queue_pr_view"),
            "error must point at the queue method: {io}"
        );
    }

    #[tokio::test]
    async fn call_record_captures_args_in_invocation_order() {
        let mock = MockGhClient::new();
        mock.queue_pr_view(Ok(ok_view("OPEN")));
        mock.queue_pr_view(Ok(ok_view("OPEN")));

        mock.pr_view(args_for_pr(101)).await.unwrap();
        mock.pr_view(args_for_pr(202)).await.unwrap();

        assert_eq!(mock.pr_view_call_count(), 2);
        let received = mock.received_pr_view_calls();
        assert_eq!(received[0].number, 101);
        assert_eq!(received[1].number, 202);
    }

    #[tokio::test]
    async fn queued_pr_merge_can_return_typed_error() {
        let mock = MockGhClient::new();
        mock.queue_pr_merge(Err(GhError::PrNotMergeable {
            stderr: "conflicts".to_string(),
        }));

        let result = mock
            .pr_merge(PrMergeArgs {
                repo: "CambrianTech/airc".to_string(),
                number: 99,
            })
            .await;

        assert!(matches!(result, Err(GhError::PrNotMergeable { .. })));
        assert_eq!(mock.pr_merge_call_count(), 1);
    }

    #[tokio::test]
    async fn each_method_has_independent_queue_and_call_record() {
        let mock = MockGhClient::new();
        mock.queue_pr_view(Ok(ok_view("OPEN")));
        mock.queue_branch_check_rollup(Ok(vec![GhCheck {
            conclusion: Some("FAILURE".to_string()),
            status: Some("COMPLETED".to_string()),
            name: Some("flaky-base-check".to_string()),
            started_at: None,
        }]));

        mock.pr_view(args_for_pr(1)).await.unwrap();
        mock.branch_check_rollup(BranchCheckRollupArgs {
            repo: "CambrianTech/airc".to_string(),
            branch: "rust-rewrite".to_string(),
        })
        .await
        .unwrap();

        // Independent: pr_create's queue is still empty, the
        // call record is still zero.
        assert_eq!(mock.pr_view_call_count(), 1);
        assert_eq!(mock.branch_check_rollup_call_count(), 1);
        assert_eq!(mock.pr_create_call_count(), 0);
    }

    /// Trait-object usage — exercises that `Arc<dyn GhClient>`
    /// works against the mock. This is the merger / work_commands
    /// shape: code holds `&dyn GhClient`, tests inject
    /// `Arc::new(MockGhClient::new())`.
    #[tokio::test]
    async fn dyn_gh_client_dispatches_through_mock() {
        use std::sync::Arc;
        let mock = Arc::new(MockGhClient::new());
        mock.queue_pr_view(Ok(ok_view("OPEN")));

        let client: Arc<dyn GhClient> = mock.clone();
        let view = client.pr_view(args_for_pr(7)).await.unwrap();

        assert_eq!(view.state, "OPEN");
        assert_eq!(mock.pr_view_call_count(), 1);
    }
}
