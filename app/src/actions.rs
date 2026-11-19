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
