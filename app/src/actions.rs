//! Jira Desk's named actions. Keys live in `keymap/default.json`; the help overlay lists them
//! from there, so the two never disagree.

use gpui::actions;

actions!(
    jdesk,
    [
        // Whole app.
        Refresh,
        FullRefresh,
        ApplyAll,
        GoToIssue,
        ShowHelp,
        /// Every command, searchable by name, with its key (⇧⌘P or Shift twice).
        ShowPalette,
        /// Copy the text selected in the issue pane.
        CopySelection,
        /// Copy it with a line naming the issue and its link (as Chorong's Copy with Location).
        CopyWithLink,
        /// Select all the issue pane's text.
        SelectAllText,
        /// Auto-advance on or off, saved in prefs.json (over `auto_advance` in config.toml).
        ToggleAutoAdvance,
        /// In the search bar: open a chip for the field word just typed, or take the
        /// highlighted value.
        SearchTab,
        /// In the search bar: reopen the last chip.
        SearchReopen,
        Quit,
        FocusFilters,
        FocusIssues,
        FocusPreview,
        FocusNotes,
        FocusChanges,
        OpenBoard,
        // Moving in the focused pane.
        CursorDown,
        CursorUp,
        CursorLeft,
        CursorRight,
        CursorTop,
        CursorBottom,
        PageDown,
        PageUp,
        Confirm,
        Toggle,
        /// Shift-Space in Filters: every sprint from the last one chosen to the one here.
        ToggleRange,
        /// A request on the current issue: something to make true of it outside Jira Desk.
        AddRequest,
        /// The current issue's newest open request, as text for an agent (and handed off).
        CopyRequestForAgent,
        /// Only issues with open requests.
        ToggleRequestsOnly,
        /// Link the current issue to another: how, then which (queued).
        LinkIssue,
        /// Put the current issue under an epic (queued).
        SetParent,
        /// Open the current issue in a new pane to the right of the issue panes (⌘Enter).
        OpenIssueRight,
        /// …or below them (⌘⇧Enter).
        OpenIssueBelow,
        /// Choose which sprints the Filters pane lists.
        ChooseShownSprints,
        /// h on a sprint row in Filters: hide it there (Choose which sprints show… brings it
        /// back).
        HideSprintRow,
        // Filters.
        Search,
        FilterSprint,
        FilterAssignee,
        FilterLabel,
        FilterEpic,
        FilterStatus,
        ToggleMine,
        ToggleHideDone,
        ToggleStarredOnly,
        ToggleNotedOnly,
        TogglePendingOnly,
        ClearFilters,
        ChooseSort,
        // The current issue (queued, never sent until applied).
        Transition,
        Assign,
        AssignToMe,
        EditLabels,
        Comment,
        Rename,
        SetPoints,
        SetPriority,
        MoveToSprint,
        UndoChange,
        // Local only.
        ToggleStar,
        EditNote,
        EditTags,
        FilterTags,
        ToggleReviewOnly,
        // A focus series: pick issues, then open them one per focus page.
        TogglePick,
        ClearPicks,
        FocusSeries,
        // An issue that changed a lot in Jira: same work, or repurposed?
        ResolveReview,
        // Settings and the storage meter.
        ShowSettings,
        MeasureStorage,
        ClearImages,
        CopyDataPath,
        // Leaving the app.
        OpenIssueTab,
        OpenInBrowser,
        CopyKey,
        CopyLink,
        // The Changes pane.
        ApplySelected,
        DiscardChange,
        RetryChange,
        ForceChange,
        ClearHistory,
        // Text fields, pickers and prompts (key contexts `JiraInput` and `JiraText`).
        InputCancel,
        InputConfirm,
        /// ⇧Enter: as Enter, except that adding issues to a focus page puts this one below.
        InputConfirmAlt,
        InputSubmit,
        InputUp,
        InputDown,
    ]
);

/// The List's status chip N, counted from 1 in config.toml's order (`[[status_group]]`). In a
/// keymap: `["jdesk::ToggleStatusGroup", 2]`.
#[derive(Clone, PartialEq, Debug, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = jdesk)]
pub struct ToggleStatusGroup(pub usize);
