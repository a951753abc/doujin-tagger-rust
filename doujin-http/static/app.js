(() => {
  "use strict";

  const RECENT_KEY = "doujin-library.recent.v1";
  const LAYOUT_KEY = "doujin-library.layout.v1";
  const EXTERNAL_JOB_KEY = "doujin-library.external-jobs.v1";
  const RECENT_LIMIT = 20;
  const PER_PAGE = 48;
  const SHELF_LIMIT = 8;
  const SAVED_VIEW_SHELF_LIMIT = 6;
  const BATCH_REQUEST_SIZE = 100;
  const COLLECTION_WINDOW_SIZE = 384;
  const COLLECTION_WINDOW_OVERSCAN = 96;
  const THUMBNAIL_REQUEST_CONCURRENCY = 4;
  const THUMBNAIL_POLL_DELAYS = [1000, 2000, 3000, 5000];
  const THUMBNAIL_NETWORK_DELAYS = [1000, 2000, 5000, 10000, 30000];
  const FILTER_NAMES = ["source", "classification", "missing", "event", "circle", "author", "parody", "subcategory", "tag", "untagged"];
  const FILTER_LABELS = {
    q: "搜尋",
    source: "來源",
    classification: "種類",
    missing: "缺少資料",
    event: "場次",
    circle: "社團",
    author: "作者",
    parody: "原作",
    subcategory: "子分類",
    tag: "標籤",
    untagged: "尚無標籤",
  };
  const METADATA_LABELS = {
    title: "標題",
    event: "場次",
    circle: "社團",
    authors: "作者",
    parody: "原作",
    classification: "種類",
    is_dl: "DL 版",
  };
  const METADATA_SOURCE_LABELS = {
    manual: "手動修改",
    legacy: "舊版匯入",
    external: "外部 metadata",
    filename: "檔名解析",
    inference: "推斷結果",
  };
  const ASSERTION_STATUS_LABELS = {
    candidate: "待裁決",
    accepted: "可用",
    rejected: "已拒絕",
    obsolete: "已失效",
  };
  const SELECTION_KIND_LABELS = {
    priority: "來源優先序",
    manual: "人工選擇",
    migration: "遷移保留",
  };
  const SEARCH_DISPOSITION_LABELS = {
    search_only: "僅供追查",
    suggestion: "建議候選",
    auto_applied: "自動套用",
  };
  const EXTERNAL_JOB_STATUS_LABELS = {
    pending: "等待搜尋",
    running: "搜尋中",
    succeeded: "搜尋完成",
    partial: "部分完成",
    failed: "搜尋失敗",
  };
  const SCAN_ISSUE_KIND_LABELS = {
    no_roots: "沒有來源",
    missing_root: "來源不存在",
    read_directory: "無法讀取資料夾",
    read_entry: "無法讀取項目",
    non_unicode_filename: "檔名編碼無效",
    ingest: "無法寫入目錄",
    reconcile: "無法核對既有收藏",
  };

  const state = {
    route: "shelf",
    page: 1,
    totalPages: 0,
    total: 0,
    items: [],
    selected: null,
    filters: {},
    filterTags: [],
    libraryDataKey: null,
    libraryRouteHash: "#library",
    libraryScrollY: 0,
    libraryRestorePage: 1,
    libraryFocusId: null,
    outOfQueryCollection: null,
    restoreLibraryContext: false,
    leavingLibraryContextCaptured: false,
    selectedIds: new Set(),
    selectedRecords: new Map(),
    sort: "created",
    direction: "desc",
    layout: readStorage(LAYOUT_KEY, "grid"),
    recent: readStorage(RECENT_KEY, []),
    requestNumber: 0,
    statsLoaded: false,
    statsData: null,
    shelfLoaded: false,
    shelfData: null,
    savedViewsLoaded: false,
    savedViews: [],
    savedViewsPromise: null,
    activeSavedViewId: null,
    savedViewDialogMode: null,
    libraryLoaded: false,
    libraryLoading: false,
    libraryLoadError: false,
    libraryEmptyContext: null,
    workbenchLoaded: false,
    reviewLoaded: false,
    reviewLoading: false,
    reviewItems: [],
    reviewTotal: 0,
    reviewPage: 1,
    reviewTotalPages: 0,
    reviewPosition: 0,
    reviewKind: "all",
    reviewSkipped: new Set(),
    reviewRequestNumber: 0,
    reviewReturnId: null,
    candidates: [],
    preflight: null,
    preflightPair: null,
    metadataHistoryCollectionId: null,
    metadataHistory: null,
    metadataRequestNumber: 0,
    openMetadataFields: new Set(),
    externalJobRefs: readStorage(EXTERNAL_JOB_KEY, {}),
    externalJob: null,
    externalJobTimer: null,
    externalBatch: null,
    externalBatchPreflight: null,
    externalBatchTimer: null,
    serviceOnline: null,
    activityExternalJobs: new Map(),
    activityScan: null,
    activityThumbnailFailures: new Set(),
    thumbnailCacheJob: null,
    thumbnailCachePreflight: null,
    thumbnailCacheRetrying: false,
    thumbnailCacheTimer: null,
    settingsRoots: [],
    settingsSnapshot: null,
    rootsNeedScan: false,
    settingsRootFocus: null,
    selectionContext: null,
    lastBatchActivity: null,
    batchRetry: null,
    batchRunning: null,
    activityTimer: null,
    activitySignature: null,
    scanPreflight: null,
    scanRequest: null,
    metadataEditCollection: null,
  };

  if (!Array.isArray(state.recent)) state.recent = [];
  if (!['list', 'grid'].includes(state.layout)) state.layout = 'list';
  if (!state.externalJobRefs || typeof state.externalJobRefs !== "object" || Array.isArray(state.externalJobRefs)) state.externalJobRefs = {};

  const ui = {};
  const mobileDetailMedia = window.matchMedia("(max-width: 899px)");
  const facetControllers = new Map();
  const tagSuggestionControllers = new Map();
  const thumbnailBindings = new WeakMap();
  const thumbnailTrackers = new Map();
  const thumbnailRequestQueue = [];
  let libraryLoadPromise = null;
  let thumbnailRequestsInFlight = 0;
  let lastThumbnailRequestEpoch = 0;
  let lastThumbnailStatusId = 0;
  let mobileDetailReturnId = null;
  let mobileDetailScrollPosition = 0;
  let mobileDetailRestoreFocus = true;
  let libraryScrollObserver = null;
  let collectionWindowStart = 0;
  let collectionWindowEnd = 0;
  let collectionWindowFrame = null;
  const collectionRowHeights = { grid: 0, list: 0 };
  let restoreFilterToggleFocus = false;
  const thumbnailObserver = typeof window.IntersectionObserver === "function"
    ? new IntersectionObserver((entries) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          thumbnailObserver.unobserve(entry.target);
          activateThumbnailElement(entry.target);
        });
      }, { rootMargin: "800px 0px" })
    : null;

  document.addEventListener("DOMContentLoaded", init);

  function init() {
    Object.assign(ui, {
      serviceState: byId("service-state"),
      activityTrigger: byId("activity-trigger"),
      activitySummary: byId("activity-summary"),
      activityCount: byId("activity-count"),
      activityPanel: byId("activity-panel"),
      activityService: byId("activity-service"),
      activityServiceLabel: byId("activity-service-label"),
      activityServiceAdvice: byId("activity-service-advice"),
      activityList: byId("activity-list"),
      activityEmpty: byId("activity-empty"),
      activityAnnouncer: byId("activity-announcer"),
      shelfLoading: byId("shelf-loading"),
      shelfContent: byId("shelf-content"),
      recentShelfBooks: byId("recent-shelf-books"),
      featuredShelfBooks: byId("featured-shelf-books"),
      eventShelfBooks: byId("event-shelf-books"),
      savedViewList: byId("saved-view-list"),
      searchForm: byId("search-form"),
      searchInput: byId("search-input"),
      headerSearchScope: byId("header-search-scope"),
      filterPanel: byId("filter-panel"),
      filterToggle: byId("filter-toggle"),
      filterDraftStatus: byId("filter-draft-status"),
      applyFilters: byId("apply-filters"),
      activeFilterCount: byId("active-filter-count"),
      activeFilterChips: byId("active-filter-chips"),
      filterTagChips: byId("filter-tag-chips"),
      results: byId("collection-results"),
      loading: byId("library-loading"),
      empty: byId("library-empty"),
      emptySymbol: byId("library-empty-symbol"),
      emptyHeading: byId("library-empty-heading"),
      emptyDescription: byId("library-empty-description"),
      emptyContext: byId("library-empty-context"),
      emptyPrimary: byId("library-empty-primary"),
      emptySecondary: byId("library-empty-secondary"),
      resultSummary: byId("result-summary"),
      librarySort: byId("library-sort"),
      savedViewContext: byId("saved-view-context"),
      savedViewActiveName: byId("saved-view-active-name"),
      savedViewDirty: byId("saved-view-dirty"),
      saveCurrentView: byId("save-current-view"),
      updateSavedView: byId("update-saved-view"),
      saveAsView: byId("save-as-view"),
      renameSavedView: byId("rename-saved-view"),
      deleteSavedView: byId("delete-saved-view"),
      savedViewDialog: byId("saved-view-dialog"),
      savedViewForm: byId("saved-view-form"),
      savedViewDialogHeading: byId("saved-view-dialog-heading"),
      savedViewDialogIntro: byId("saved-view-dialog-intro"),
      savedViewName: byId("saved-view-name"),
      savedViewRuleSummary: byId("saved-view-rule-summary"),
      confirmSavedView: byId("confirm-saved-view"),
      loadMore: byId("library-load-more"),
      loadMoreSpinner: byId("library-load-more-spinner"),
      loadMoreLabel: byId("library-load-more-label"),
      retryLibraryLoad: byId("retry-library-load"),
      libraryLoadAnnouncer: byId("library-load-announcer"),
      libraryScrollSentinel: byId("library-scroll-sentinel"),
      detailPane: byId("detail-pane"),
      detailPlaceholder: byId("detail-placeholder"),
      collectionDetail: byId("collection-detail"),
      mobileDetailDialog: byId("mobile-detail-dialog"),
      mobileDetailContent: byId("mobile-detail-content"),
      mobileDetailClose: byId("close-mobile-detail"),
      detailCover: byId("detail-cover"),
      detailSource: byId("detail-source"),
      detailKicker: byId("detail-kicker"),
      detailTitle: byId("detail-title"),
      detailFilename: byId("detail-filename"),
      metadataList: byId("metadata-list"),
      missingMetadataActions: byId("missing-metadata-actions"),
      metadataEvidence: byId("metadata-evidence"),
      dataQualitySummary: byId("data-quality-summary"),
      evidenceSummaryCount: byId("evidence-summary-count"),
      evidenceLoading: byId("evidence-loading"),
      evidenceError: byId("evidence-error"),
      evidenceErrorMessage: byId("evidence-error-message"),
      evidenceFields: byId("evidence-fields"),
      externalJobStatus: byId("external-job-status"),
      detailTags: byId("detail-tags"),
      detailPath: byId("detail-path"),
      tagForm: byId("tag-form"),
      tagInput: byId("tag-input"),
      recentDialog: byId("recent-dialog"),
      recentList: byId("recent-list"),
      recentCount: byId("recent-count"),
      focusFilterDialog: byId("focus-filter-dialog"),
      focusFilterMessage: byId("focus-filter-message"),
      discardFilterDialog: byId("discard-filter-dialog"),
      metadataDialog: byId("metadata-dialog"),
      metadataForm: byId("metadata-form"),
      metadataField: byId("metadata-field"),
      metadataTextGroup: byId("metadata-text-group"),
      metadataValue: byId("metadata-value"),
      metadataValueLabel: byId("metadata-value-label"),
      metadataClassificationGroup: byId("metadata-classification-group"),
      metadataBooleanGroup: byId("metadata-boolean-group"),
      statLedger: byId("stat-ledger"),
      statColumns: byId("stat-columns"),
      settingsForm: byId("settings-form"),
      environmentOverrides: byId("environment-overrides"),
      viewerPathOverride: byId("viewer-path-override"),
      thumbSizeOverride: byId("thumb-size-override"),
      thumbQualityOverride: byId("thumb-quality-override"),
      thumbnailCacheForm: byId("thumbnail-cache-form"),
      thumbnailCacheRoots: byId("thumbnail-cache-roots"),
      thumbnailCacheStart: byId("thumbnail-cache-start"),
      thumbnailCacheProgress: byId("thumbnail-cache-progress"),
      thumbnailCacheStatus: byId("thumbnail-cache-status"),
      thumbnailCachePercent: byId("thumbnail-cache-percent"),
      thumbnailCacheProgressBar: byId("thumbnail-cache-progress-bar"),
      thumbnailCacheDetail: byId("thumbnail-cache-detail"),
      thumbnailCacheActions: byId("thumbnail-cache-actions"),
      thumbnailCacheViewFailures: byId("thumbnail-cache-view-failures"),
      thumbnailCacheRetryFailures: byId("thumbnail-cache-retry-failures"),
      thumbnailCachePreflightDialog: byId("thumbnail-cache-preflight-dialog"),
      thumbnailCacheConfirmForm: byId("thumbnail-cache-confirm-form"),
      thumbnailCachePreflightSummary: byId("thumbnail-cache-preflight-summary"),
      thumbnailCachePreflightRoots: byId("thumbnail-cache-preflight-roots"),
      thumbnailCacheConfirm: byId("thumbnail-cache-confirm"),
      rootList: byId("root-list"),
      rootForm: byId("root-form"),
      rootsHeading: byId("roots-heading"),
      rootRescanNote: byId("root-rescan-note"),
      editRootDialog: byId("edit-root-dialog"),
      editRootForm: byId("edit-root-form"),
      scanButton: byId("scan-button"),
      scanPreflightDialog: byId("scan-preflight-dialog"),
      scanPreflightForm: byId("scan-preflight-form"),
      scanPreflightSummary: byId("scan-preflight-summary"),
      scanPreflightDetails: byId("scan-preflight-details"),
      scanPreflightRenamesSection: byId("scan-preflight-renames-section"),
      scanPreflightRenames: byId("scan-preflight-renames"),
      scanPreflightWarningsSection: byId("scan-preflight-warnings-section"),
      scanPreflightWarnings: byId("scan-preflight-warnings"),
      scanPreflightTombstonesSection: byId("scan-preflight-tombstones-section"),
      scanPreflightTombstones: byId("scan-preflight-tombstones"),
      scanPreflightConfirm: byId("scan-preflight-confirm"),
      scanResultsDialog: byId("scan-results-dialog"),
      scanResultsSummary: byId("scan-results-summary"),
      scanIssueList: byId("scan-issue-list"),
      scanResultsEmpty: byId("scan-results-empty"),
      scanResultsRetry: byId("scan-results-retry"),
      toastRegion: byId("toast-region"),
      selectionRail: byId("selection-rail"),
      selectionCount: byId("selection-count"),
      selectionWorkbenchLink: byId("selection-workbench-link"),
      workbenchCount: byId("workbench-count"),
      reviewCount: byId("review-count"),
      reviewTotal: byId("review-total"),
      reviewPosition: byId("review-position"),
      reviewKind: byId("review-kind"),
      reviewLoading: byId("review-loading"),
      reviewError: byId("review-error"),
      reviewErrorMessage: byId("review-error-message"),
      reviewEmpty: byId("review-empty"),
      reviewEmptyMessage: byId("review-empty-message"),
      resetReviewSkips: byId("reset-review-skips"),
      reviewDesk: byId("review-desk"),
      reviewCover: byId("review-cover"),
      reviewSource: byId("review-source"),
      reviewSequence: byId("review-sequence"),
      reviewTitle: byId("review-title"),
      reviewFilename: byId("review-filename"),
      reviewContext: byId("review-context"),
      reviewProblems: byId("review-problems"),
      reviewDecision: byId("review-decision"),
      reviewMoreEvidence: byId("review-more-evidence"),
      reviewAllIssues: byId("review-all-issues"),
      reviewAccept: byId("review-accept"),
      reviewReject: byId("review-reject"),
      reviewEdit: byId("review-edit"),
      reviewSkip: byId("review-skip"),
      reviewDetail: byId("review-detail"),
      reviewPrevious: byId("review-previous"),
      reviewNext: byId("review-next"),
      workbenchSelectionSummary: byId("workbench-selection-summary"),
      selectedCollectionList: byId("selected-collection-list"),
      selectionEmpty: byId("selection-empty"),
      batchTools: byId("batch-tools"),
      batchTagForm: byId("batch-tag-form"),
      batchMetadataForm: byId("batch-metadata-form"),
      batchResult: byId("batch-result"),
      batchResultSummary: byId("batch-result-summary"),
      batchResultItems: byId("batch-result-items"),
      batchProgress: byId("batch-progress"),
      batchProgressLabel: byId("batch-progress-label"),
      batchProgressCount: byId("batch-progress-count"),
      batchProgressBar: byId("batch-progress-bar"),
      retryBatchFailures: byId("retry-batch-failures"),
      externalBatchPreflight: byId("external-batch-preflight"),
      externalBatchActions: byId("external-batch-actions"),
      externalBatchResult: byId("external-batch-result"),
      moveDialog: byId("move-dialog"),
      moveForm: byId("move-form"),
      archiveRootSelect: byId("archive-root-select"),
      deleteDialog: byId("delete-dialog"),
      deleteForm: byId("delete-form"),
      permanentConfirmGroup: byId("permanent-confirm-group"),
      permanentConfirmPhrase: byId("permanent-confirm-phrase"),
      candidateLoading: byId("candidate-loading"),
      candidateGroups: byId("candidate-groups"),
      candidateEmpty: byId("candidate-empty"),
      identityResult: byId("identity-result"),
      consolidationDialog: byId("consolidation-dialog"),
      consolidationForm: byId("consolidation-form"),
      preflightBlockers: byId("preflight-blockers"),
      conflictSection: byId("conflict-section"),
      conflictList: byId("conflict-list"),
      consolidationConfirmPhrase: byId("consolidation-confirm-phrase"),
      confirmConsolidation: byId("confirm-consolidation"),
    });

    bindEvents();
    initializeLibraryInfiniteScroll();
    renderRecent();
    setLayout(state.layout);
    routeFromHash();
    loadSavedViews();
    startActivityMonitoring();
  }

  function bindEvents() {
    initializeFacetComboboxes();
    initializeTagSuggestionInputs();
    window.addEventListener("hashchange", routeFromHash);
    ui.activityTrigger.addEventListener("click", () => setActivityPanelOpen(ui.activityPanel.hidden));
    byId("close-activity").addEventListener("click", () => setActivityPanelOpen(false));
    byId("refresh-activity").addEventListener("click", () => refreshActivityCenter(true));
    document.addEventListener("pointerdown", (event) => {
      if (ui.activityPanel.hidden || ui.activityPanel.contains(event.target) || ui.activityTrigger.contains(event.target)) return;
      setActivityPanelOpen(false);
    });
    document.querySelectorAll("[data-route]").forEach((link) => {
      link.addEventListener("click", (event) => {
        if (state.route !== "library" && link.dataset.route === "library") {
          event.preventDefault();
          location.hash = state.libraryRouteHash;
          return;
        }
        if (state.route !== "library" || link.dataset.route === "library") return;
        rememberLibraryContext();
        state.libraryRouteHash = location.hash || libraryHash();
        updateLibraryNavHref();
        state.leavingLibraryContextCaptured = true;
      });
    });
    byId("return-to-library-context").addEventListener("click", (event) => {
      event.preventDefault();
      returnToLibraryContext();
    });
    ui.searchForm.addEventListener("submit", (event) => {
      event.preventDefault();
      const filterSubmission = event.submitter === ui.applyFilters || ui.filterPanel.contains(document.activeElement);
      if (filterSubmission) applyFilterDraft();
      else applyHeaderSearch();
    });
    ui.filterPanel.addEventListener("input", updateFilterDraftState);
    ui.filterPanel.addEventListener("change", updateFilterDraftState);
    ui.filterToggle.addEventListener("click", () => {
      if (ui.filterPanel.hidden) setFilterPanelOpen(true);
      else requestFilterPanelClose({ restoreFocus: true });
    });
    byId("close-filter-panel").addEventListener("click", () => requestFilterPanelClose({ restoreFocus: true }));
    document.addEventListener("pointerdown", (event) => {
      if (ui.filterPanel.hidden || ui.filterPanel.contains(event.target) || ui.filterToggle.contains(event.target)) return;
      requestFilterPanelClose();
    });
    byId("clear-filters").addEventListener("click", clearAppliedFilters);
    ui.emptyPrimary.addEventListener("click", handleLibraryEmptyPrimary);
    ui.emptySecondary.addEventListener("click", handleLibraryEmptySecondary);
    byId("discard-filter-changes").addEventListener("click", discardFilterChanges);
    ui.retryLibraryLoad.addEventListener("click", loadMoreCollections);
    document.querySelectorAll("[data-layout]").forEach((button) => {
      button.addEventListener("click", () => setLayout(button.dataset.layout));
    });
    ui.librarySort.addEventListener("change", changeLibrarySort);
    ui.saveCurrentView.addEventListener("click", () => openSavedViewDialog("create"));
    ui.updateSavedView.addEventListener("click", updateActiveSavedView);
    ui.saveAsView.addEventListener("click", () => openSavedViewDialog("save-as"));
    ui.renameSavedView.addEventListener("click", () => openSavedViewDialog("rename"));
    ui.deleteSavedView.addEventListener("click", deleteActiveSavedView);
    ui.savedViewForm.addEventListener("submit", submitSavedViewDialog);
    byId("read-button").addEventListener("click", () => launchSelected("read"));
    byId("open-button").addEventListener("click", () => launchSelected("open"));
    byId("edit-metadata-button").addEventListener("click", () => openMetadataDialog());
    byId("external-search-button").addEventListener("click", enqueueExternalSearch);
    byId("rebuild-thumbnail-button").addEventListener("click", rebuildThumbnail);
    ui.tagForm.addEventListener("submit", addTag);
    ui.metadataEvidence.addEventListener("toggle", toggleMetadataEvidence);
    byId("refresh-metadata-evidence").addEventListener("click", () => loadMetadataEvidence(true));
    byId("retry-metadata-evidence").addEventListener("click", () => loadMetadataEvidence(true));

    byId("recent-button").addEventListener("click", () => {
      renderRecent();
      ui.recentDialog.showModal();
    });
    byId("shortcuts-button").addEventListener("click", () => byId("shortcuts-dialog").showModal());
    document.querySelectorAll(".quick-filter").forEach((button) => {
      button.addEventListener("click", () => showShelfFilter(button.dataset.filter, button.dataset.value));
    });
    document.querySelectorAll("[data-shelf-target]").forEach((button) => {
      button.addEventListener("click", () => {
        const target = button.dataset.shelfTarget;
        if (target === "event") showShelfFilter("event", state.shelfData?.eventName);
        else showShelfFilter(null, null);
      });
    });
    byId("manage-saved-views").addEventListener("click", () => {
      state.activeSavedViewId = null;
      setAppliedFilters({});
      navigateLibrary();
    });
    initializeShelfScrollControls();
    byId("clear-recent").addEventListener("click", clearRecent);
    byId("clear-filters-and-focus").addEventListener("click", clearFiltersAndFocus);
    ui.metadataField.addEventListener("change", syncMetadataEditor);
    ui.metadataForm.addEventListener("submit", saveMetadata);
    byId("clear-manual-button").addEventListener("click", clearManualMetadata);
    ui.settingsForm.addEventListener("submit", saveSettings);
    ui.thumbnailCacheForm.addEventListener("submit", startThumbnailCacheJob);
    ui.thumbnailCacheConfirmForm.addEventListener("submit", confirmThumbnailCacheJob);
    ui.thumbnailCacheViewFailures.addEventListener("click", openThumbnailCacheFailures);
    ui.thumbnailCacheRetryFailures.addEventListener("click", retryThumbnailCacheFailures);
    ui.rootForm.addEventListener("submit", registerRoot);
    ui.editRootForm.addEventListener("submit", saveEditedRoot);
    ui.scanButton.addEventListener("click", startScan);
    ui.scanPreflightForm.addEventListener("submit", applyScanPreflight);
    ui.scanResultsRetry.addEventListener("click", () => {
      ui.scanResultsDialog.close();
      startScan();
    });
    byId("select-loaded").addEventListener("click", selectLoadedCollections);
    byId("invert-loaded").addEventListener("click", invertLoadedSelection);
    byId("clear-selection").addEventListener("click", clearSelection);
    ui.batchTagForm.addEventListener("submit", batchAddTag);
    ui.batchMetadataForm.elements.field.addEventListener("change", syncBatchMetadataField);
    ui.batchMetadataForm.addEventListener("submit", batchSetMetadata);
    ui.retryBatchFailures.addEventListener("click", retryFailedBatch);
    byId("prepare-external-batch").addEventListener("click", preflightExternalBatch);
    byId("start-external-batch").addEventListener("click", startExternalBatch);
    byId("cancel-external-batch").addEventListener("click", clearExternalBatchPreflight);
    byId("prepare-move").addEventListener("click", prepareMove);
    ui.moveForm.addEventListener("submit", executeMove);
    byId("prepare-delete").addEventListener("click", prepareDelete);
    ui.deleteForm.addEventListener("change", syncDeleteMode);
    ui.deleteForm.addEventListener("input", syncDeleteMode);
    ui.deleteForm.addEventListener("submit", executeDelete);
    byId("refresh-candidates").addEventListener("click", loadTombstoneCandidates);
    byId("refresh-review").addEventListener("click", () => loadReviewQueue({ preferredId: currentReviewItem()?.collection.id }));
    byId("retry-review").addEventListener("click", () => loadReviewQueue({ preferredId: currentReviewItem()?.collection.id }));
    ui.reviewKind.addEventListener("change", () => {
      state.reviewKind = ui.reviewKind.value;
      state.reviewPage = 1;
      state.reviewPosition = 0;
      loadReviewQueue();
    });
    ui.resetReviewSkips.addEventListener("click", resetReviewSkips);
    ui.reviewAccept.addEventListener("click", () => decideReviewCandidate("select"));
    ui.reviewReject.addEventListener("click", () => decideReviewCandidate("reject"));
    ui.reviewEdit.addEventListener("click", openReviewEditor);
    ui.reviewSkip.addEventListener("click", skipCurrentReviewItem);
    ui.reviewDetail.addEventListener("click", openReviewDetail);
    ui.reviewPrevious.addEventListener("click", () => moveReviewPosition(-1));
    ui.reviewNext.addEventListener("click", () => moveReviewPosition(1));
    ui.consolidationForm.addEventListener("input", syncConsolidationConfirmation);
    ui.consolidationForm.addEventListener("change", syncConsolidationConfirmation);
    ui.consolidationForm.addEventListener("submit", executeConsolidation);
    ui.mobileDetailDialog.addEventListener("close", finishMobileDetailClose);
    mobileDetailMedia.addEventListener("change", (event) => {
      if (!event.matches) closeMobileDetail({ restoreFocus: false });
    });

    document.querySelectorAll("[data-close-dialog]").forEach((button) => {
      button.addEventListener("click", () => button.closest("dialog")?.close());
    });
    document.querySelectorAll("dialog").forEach((dialog) => {
      dialog.addEventListener("click", (event) => {
        if (event.target === dialog) dialog.close();
      });
    });
    ui.metadataDialog.addEventListener("close", () => { state.metadataEditCollection = null; });
    document.addEventListener("keydown", handleKeyboard);
  }

  function setFilterPanelOpen(open, { restoreFocus = false } = {}) {
    if (open) syncFilterDraftFromApplied();
    ui.filterPanel.hidden = !open;
    ui.filterToggle.setAttribute("aria-expanded", String(open));
    if (open) ui.filterPanel.querySelector("select, input")?.focus();
    else {
      closeAllFacetOptions();
      if (restoreFocus) ui.filterToggle.focus();
    }
  }

  function requestFilterPanelClose({ restoreFocus = false } = {}) {
    if (filterDraftChanged()) {
      restoreFilterToggleFocus = restoreFocus;
      if (!ui.discardFilterDialog.open) ui.discardFilterDialog.showModal();
      return false;
    }
    setFilterPanelOpen(false, { restoreFocus });
    return true;
  }

  function discardFilterChanges() {
    ui.discardFilterDialog.close();
    syncFilterDraftFromApplied();
    setFilterPanelOpen(false, { restoreFocus: restoreFilterToggleFocus });
    restoreFilterToggleFocus = false;
  }

  function routeFromHash() {
    const previousRoute = state.route;
    const parsedRoute = parseRouteHash();
    const route = parsedRoute.route;
    const nextRoute = ["shelf", "library", "review", "workbench", "stats", "settings"].includes(route) ? route : "shelf";
    if (previousRoute === "library" && nextRoute !== "library") {
      if (!state.leavingLibraryContextCaptured) rememberLibraryContext();
      state.leavingLibraryContextCaptured = false;
    }
    let libraryNeedsLoad = false;
    let preserveLibrarySelection = false;
    let libraryFocusChanged = false;
    if (nextRoute === "library") {
      const decoded = decodeLibraryParams(parsedRoute.params);
      const dataChanged = state.libraryDataKey !== decoded.dataKey;
      libraryFocusChanged = decoded.focusId !== state.libraryFocusId;
      const focusOutsideLoaded = decoded.focusId && !state.items.some((item) => item.id === decoded.focusId);
      if (focusOutsideLoaded && previousRoute === "library") rememberLibraryContext();
      libraryNeedsLoad = dataChanged || !state.libraryLoaded || focusOutsideLoaded;
      preserveLibrarySelection = libraryNeedsLoad && !dataChanged;
      if (dataChanged && state.selectedIds.size > 0 && !confirmSelectionClear()) {
        const rollbackHash = previousRoute === "library" ? state.libraryRouteHash : `#${previousRoute}`;
        history.replaceState(null, "", rollbackHash);
        if (previousRoute === "library") {
          applyDecodedLibraryState(decodeLibraryParams(parseRouteHash().params));
        }
        return Promise.resolve(false);
      }
      if (dataChanged) {
        state.libraryScrollY = 0;
        state.libraryRestorePage = 1;
      }
      applyDecodedLibraryState(decoded);
      state.libraryRouteHash = location.hash || "#library";
      updateLibraryNavHref();
      state.restoreLibraryContext = previousRoute !== "library" || libraryNeedsLoad;
      renderSavedViewContext();
    }
    state.route = nextRoute;
    if (state.route !== "settings") stopThumbnailCachePolling();
    if (state.route !== "library") closeMobileDetail({ restoreFocus: false });
    if (state.route !== "library" && !ui.filterPanel.hidden) {
      const discardedDraft = filterDraftChanged();
      syncFilterDraftFromApplied();
      setFilterPanelOpen(false);
      if (discardedDraft) toast("已放棄尚未套用的篩選變更");
    }
    ui.headerSearchScope.disabled = state.route !== "library";
    if (ui.headerSearchScope.disabled) ui.headerSearchScope.value = "all";
    document.documentElement.dataset.route = state.route;
    document.querySelectorAll("[data-view]").forEach((view) => {
      const active = view.dataset.view === state.route;
      view.hidden = !active;
      if (active) resumeThumbnailsWithin(view);
      else pauseThumbnailsWithin(view);
    });
    document.querySelectorAll("[data-route]").forEach((link) => {
      if (link.dataset.route === state.route) link.setAttribute("aria-current", "page");
      else link.removeAttribute("aria-current");
    });
    if (state.route === "shelf") loadShelf();
    let libraryReady = Promise.resolve(true);
    if (state.route === "library" && libraryNeedsLoad) {
      libraryReady = loadCollections({
        preserveSelection: preserveLibrarySelection,
        restoreThroughPage: state.libraryRestorePage,
      });
    } else if (state.route === "library") {
      const focused = applyLibraryFocus();
      if (!focused) clearDetail();
      if (state.restoreLibraryContext) restoreLibraryWorkContext();
      else if (focused && libraryFocusChanged) revealFocusedCollection();
      scheduleLibraryLoadCheck();
    }
    if (state.route === "workbench") loadWorkbench();
    if (state.route === "review") {
      const preferredId = state.reviewReturnId || currentReviewItem()?.collection.id;
      state.reviewReturnId = null;
      loadReviewQueue({ preferredId });
    }
    if (state.route === "stats") loadStats();
    if (state.route === "settings") loadSettingsPage();
    if (state.route !== "library") window.scrollTo({ top: 0, behavior: "auto" });
    document.title = `${routeTitle(state.route)}｜私藏編目室`;
    return libraryReady;
  }

  function parseRouteHash() {
    const hash = location.hash.slice(1);
    const separator = hash.indexOf("?");
    const route = (separator >= 0 ? hash.slice(0, separator) : hash) || "shelf";
    const query = separator >= 0 ? hash.slice(separator + 1) : "";
    return { route, params: new URLSearchParams(query) };
  }

  function decodeLibraryParams(params) {
    const values = {};
    const query = String(params.get("q") || "").trim();
    if (query) values.q = query;
    FILTER_NAMES.forEach((name) => {
      if (name === "tag") return;
      const value = String(params.get(name) || "").trim();
      if (value) values[name] = value;
    });
    const tags = params.getAll("tag").map((tag) => tag.trim()).filter(Boolean);
    if (tags.length) values.tag = tags;
    const sort = ["created", "updated", "title"].includes(params.get("sort")) ? params.get("sort") : "created";
    const direction = ["asc", "desc"].includes(params.get("direction")) ? params.get("direction") : "desc";
    const focusId = Number.parseInt(params.get("focus") || "", 10);
    const savedViewId = Number.parseInt(params.get("view") || "", 10);
    const dataParams = libraryParams(values, null, sort, direction, null);
    return {
      values,
      tags,
      sort,
      direction,
      focusId: Number.isSafeInteger(focusId) && focusId > 0 ? focusId : null,
      savedViewId: Number.isSafeInteger(savedViewId) && savedViewId > 0 ? savedViewId : null,
      dataKey: dataParams.toString(),
    };
  }

  function applyDecodedLibraryState(decoded) {
    state.filters = { ...decoded.values, ...(decoded.tags.length ? { tag: [...decoded.tags] } : {}) };
    state.sort = decoded.sort;
    state.direction = decoded.direction;
    ui.librarySort.value = `${state.sort}:${state.direction}`;
    state.libraryFocusId = decoded.focusId;
    state.activeSavedViewId = decoded.savedViewId;
    state.libraryDataKey = decoded.dataKey;
    syncFilterDraftFromApplied();
    updateFilterCount();
  }

  function libraryParams(
    filters = state.filters,
    focusId = state.libraryFocusId,
    sort = state.sort,
    direction = state.direction,
    savedViewId = state.activeSavedViewId,
  ) {
    const params = new URLSearchParams();
    params.set("sort", sort);
    params.set("direction", direction);
    if (filters.q) params.set("q", filters.q);
    FILTER_NAMES.forEach((name) => {
      const value = filters[name];
      if (Array.isArray(value)) value.forEach((entry) => params.append(name, entry));
      else if (value) params.set(name, value);
    });
    if (focusId) params.set("focus", String(focusId));
    if (savedViewId) params.set("view", String(savedViewId));
    return params;
  }

  function libraryHash(focusId = state.libraryFocusId) {
    const query = libraryParams(state.filters, focusId).toString();
    return `#library${query ? `?${query}` : ""}`;
  }

  function navigateLibrary({ replace = false } = {}) {
    const hash = libraryHash();
    if (location.hash === hash) return;
    if (replace) {
      history.replaceState(null, "", hash);
      state.libraryRouteHash = hash;
      updateLibraryNavHref();
      return;
    }
    location.hash = hash;
  }

  function navigateToCollection(collection) {
    const hash = libraryHash(collection.id);
    if (location.hash !== hash) {
      history[state.route === "library" ? "replaceState" : "pushState"](null, "", hash);
    }
    return routeFromHash().then((navigated) => {
      const opened = navigated && state.selected?.id === collection.id;
      if (opened) revealFocusedCollection();
      return opened;
    });
  }

  function confirmSelectionClear() {
    return window.confirm(`這會清除目前 ${formatNumber(state.selectedIds.size)} 筆批次選取。要繼續嗎？`);
  }

  function rememberLibraryContext() {
    state.libraryScrollY = window.scrollY;
    state.libraryRestorePage = Math.max(1, state.page);
    state.libraryFocusId = Number(document.activeElement?.dataset?.collectionId) || state.selected?.id || state.libraryFocusId;
  }

  function updateLibraryNavHref() {
    document.querySelector('[data-route="library"]')?.setAttribute("href", state.libraryRouteHash);
    document.querySelectorAll("[data-library-context-link]").forEach((link) => link.setAttribute("href", state.libraryRouteHash));
  }

  function returnToLibraryContext() {
    location.hash = state.libraryRouteHash;
  }

  function restoreLibraryWorkContext() {
    state.restoreLibraryContext = false;
    requestAnimationFrame(() => {
      window.scrollTo({ top: state.libraryScrollY, behavior: "auto" });
      if (!state.libraryFocusId) return;
      revealFocusedCollection({ focus: true });
    });
  }

  function revealFocusedCollection({ focus = false } = {}) {
    const focusId = state.libraryFocusId;
    if (!focusId) return;
    const focusIndex = state.items.findIndex((item) => item.id === focusId);
    const reveal = () => {
      cancelCollectionWindowUpdate();
      if (focusIndex >= 0) ensureCollectionMounted(focusIndex);
      const button = ui.results.querySelector(`[data-collection-id="${focusId}"]`);
      if (!button) return null;
      if (focus) button.focus({ preventScroll: true });
      button.scrollIntoView({ block: "nearest" });
      return button;
    };
    const button = reveal();
    window.requestAnimationFrame(() => {
      if (state.route === "library" && state.libraryFocusId === focusId) reveal();
    });
    if (mobileDetailMedia.matches) openMobileDetail(button);
  }

  function routeTitle(route) {
    return { shelf: "書架", library: "全部藏書", review: "品質審核", workbench: "工作台", stats: "統計", settings: "設定" }[route];
  }

  function startActivityMonitoring() {
    renderActivityCenter();
    refreshActivityCenter(true);
  }

  async function refreshActivityCenter(forceJobs = false) {
    if (state.activityTimer != null) window.clearTimeout(state.activityTimer);
    state.activityTimer = null;
    try {
      await api("/api/health");
      state.serviceOnline = true;
      setServiceState("online", "本機服務正常");
    } catch (_) {
      state.serviceOnline = false;
      setServiceState("offline", "本機服務無回應");
    }

    if (state.serviceOnline) {
      const storedIds = Object.values(state.externalJobRefs).map(Number).filter((id) => Number.isSafeInteger(id) && id > 0).reverse();
      const activeIds = [...state.activityExternalJobs.values()]
        .filter((job) => ["pending", "running"].includes(job.status))
        .map((job) => job.id);
      const jobIds = [...new Set([
        ...activeIds,
        ...storedIds.slice(0, forceJobs || state.activityExternalJobs.size === 0 ? 12 : 3),
      ])];
      for (let index = 0; index < jobIds.length; index += 4) {
        const jobs = await Promise.all(jobIds.slice(index, index + 4).map(async (jobId) => {
          try {
            return await api(`/api/external-search-jobs/${jobId}`);
          } catch (_) {
            return null;
          }
        }));
        jobs.filter(Boolean).forEach((job) => state.activityExternalJobs.set(job.id, job));
      }
      const [cacheJobs, latestScan] = await Promise.all([
        api("/api/thumbnail-cache-jobs/current").catch(() => null),
        api("/api/scans/latest").catch(() => null),
      ]);
      if (cacheJobs) updateThumbnailCacheJob(cacheJobs.job, { announce: false });
      if (latestScan?.scan && state.activityScan?.status !== "running") {
        state.activityScan = scanActivity(latestScan.scan);
      }
      if (state.externalBatch?.id) {
        state.externalBatch = await api(`/api/external-search-batches/${state.externalBatch.id}`).catch(() => state.externalBatch);
        renderExternalBatch(state.externalBatch);
      }
    }

    renderActivityCenter();
    const thumbnailCacheRunning = state.thumbnailCacheJob?.status === "running";
    const active = state.activityScan?.status === "running"
      || thumbnailCacheRunning
      || Boolean(state.externalBatch?.summary?.pending || state.externalBatch?.summary?.running)
      || state.batchRunning != null
      || [...state.activityExternalJobs.values()].some((job) => ["pending", "running"].includes(job.status));
    state.activityTimer = window.setTimeout(() => refreshActivityCenter(), active ? 4000 : 15000);
  }

  function setServiceState(status, label) {
    ui.serviceState.className = `service-state ${status}`;
    ui.serviceState.lastChild.textContent = ` ${label}`;
  }

  function setActivityPanelOpen(open) {
    ui.activityPanel.hidden = !open;
    ui.activityTrigger.setAttribute("aria-expanded", String(open));
    if (open) {
      renderActivityCenter();
      refreshActivityCenter(true);
      byId("close-activity").focus({ preventScroll: true });
    }
  }

  function renderActivityCenter() {
    if (!ui.activityTrigger) return;
    const jobs = [...state.activityExternalJobs.values()].sort((left, right) => right.id - left.id);
    const activeJobs = jobs.filter((job) => ["pending", "running"].includes(job.status));
    const failedJobs = jobs.filter((job) => ["partial", "failed"].includes(job.status));
    const scanNeedsAttention = ["partial", "failed"].includes(state.activityScan?.status);
    const scanRunning = state.activityScan?.status === "running";
    const thumbnailCacheRunning = state.thumbnailCacheJob?.status === "running";
    const thumbnailCacheFailures = state.thumbnailCacheJob?.status === "completed_with_errors"
      ? state.thumbnailCacheJob.failed
      : 0;
    const batchRunning = state.batchRunning != null;
    const batchFailures = state.lastBatchActivity?.failed || 0;
    const enrichmentNeedsAttention = Boolean(state.externalBatch?.summary?.partial || state.externalBatch?.summary?.failed);
    const enrichmentRunning = Boolean(state.externalBatch?.summary?.pending || state.externalBatch?.summary?.running);
    const attentionCount = failedJobs.length + state.activityThumbnailFailures.size + Number(scanNeedsAttention) + batchFailures + thumbnailCacheFailures + Number(enrichmentNeedsAttention);
    const runningCount = activeJobs.length + Number(scanRunning) + Number(thumbnailCacheRunning) + Number(batchRunning) + Number(enrichmentRunning);

    let summary = "本機服務正常";
    let mode = "is-online";
    if (state.serviceOnline == null) {
      summary = "狀態檢查中";
      mode = "is-checking";
    } else if (!state.serviceOnline) {
      summary = "本機服務離線";
      mode = "has-attention";
    } else if (attentionCount > 0) {
      summary = `${formatNumber(attentionCount)} 項需要處理`;
      mode = "has-attention";
    } else if (scanRunning) {
      summary = "掃描中";
      mode = "is-running";
    } else if (thumbnailCacheRunning) {
      summary = `縮圖快取 ${formatProgressPercent(state.thumbnailCacheJob.progress_percent)}`;
      mode = "is-running";
    } else if (batchRunning) {
      summary = `批次操作 ${formatNumber(state.batchRunning.completed)} / ${formatNumber(state.batchRunning.total)}`;
      mode = "is-running";
    } else if (enrichmentRunning) {
      summary = `外部補齊 ${formatNumber(state.externalBatch.summary.succeeded + state.externalBatch.summary.partial + state.externalBatch.summary.failed)} / ${formatNumber(state.externalBatch.summary.total)}`;
      mode = "is-running";
    } else if (activeJobs.length) {
      summary = `外部搜尋 ${formatNumber(activeJobs.length)}`;
      mode = "is-running";
    }
    ui.activityTrigger.className = `activity-trigger ${mode}`;
    ui.activitySummary.textContent = summary;
    ui.activityCount.hidden = attentionCount === 0;
    ui.activityCount.textContent = String(attentionCount);
    ui.activityTrigger.setAttribute("aria-label", `系統狀態：${summary}`);

    ui.activityService.className = `activity-service ${state.serviceOnline === false ? "offline" : state.serviceOnline ? "online" : "checking"}`;
    ui.activityServiceLabel.textContent = state.serviceOnline === false ? "本機 Rust service 無回應" : state.serviceOnline ? "本機 Rust service 正常" : "正在確認本機服務…";
    ui.activityServiceAdvice.textContent = state.serviceOnline === false ? "請確認 doujin-http 仍在執行，然後重新整理狀態。" : "";

    ui.activityList.replaceChildren();
    if (state.activityScan) {
      const scan = state.activityScan;
      const detail = scan.status === "running"
        ? "正在掃描已登記的資料夾來源；目前沒有可用的百分比。"
        : scan.message || "掃描已完成。";
      const canInspect = scan.status !== "running" && (scan.id || scan.issues?.length);
      ui.activityList.append(activityItem(
        `scan ${scan.status}`,
        scan.status === "running" ? "重新掃描進行中" : `最近一次掃描${scan.id ? ` #${scan.id}` : ""}`,
        detail,
        scan.status === "partial" ? "部分完成" : scan.status === "failed" ? "失敗" : scan.status === "succeeded" ? "完成" : "進行中",
        canInspect ? "查看掃描結果" : "查看設定",
        canInspect ? showScanResults : () => {
          setActivityPanelOpen(false);
          location.hash = "settings";
        },
      ));
    }
    [...activeJobs, ...failedJobs].slice(0, 8).forEach((job) => {
      const fields = job.fields.map((field) => METADATA_LABELS[field] || field).join("、");
      ui.activityList.append(activityItem(`external ${job.status}`, `外部資料搜尋 #${job.id}`, `${fields} · 收藏 #${job.collection_id}`, EXTERNAL_JOB_STATUS_LABELS[job.status] || job.status, "查看收藏", () => openActivityCollection(job.collection_id)));
    });
    if (state.externalBatch) {
      const batch = state.externalBatch;
      const finished = batch.summary.succeeded + batch.summary.partial + batch.summary.failed + batch.summary.skipped + batch.summary.unchanged;
      ui.activityList.append(activityItem(
        `external-batch ${enrichmentNeedsAttention ? "failed" : enrichmentRunning ? "running" : "succeeded"}`,
        `批次外部資料補齊 #${batch.id}`,
        `已完成 ${formatNumber(finished)} / ${formatNumber(batch.summary.total)} · 沿用 ${formatNumber(batch.summary.reused)} 筆既有工作`,
        batchStatusLabel(batch.summary),
        "查看工作台",
        () => { setActivityPanelOpen(false); location.hash = "workbench"; },
        enrichmentNeedsAttention ? "前往品質審核" : null,
        enrichmentNeedsAttention ? () => { setActivityPanelOpen(false); location.hash = "review"; } : null,
      ));
    }
    if (state.activityThumbnailFailures.size) {
      ui.activityList.append(activityItem("thumbnail failed", "縮圖生成失敗", `${formatNumber(state.activityThumbnailFailures.size)} 冊需要從收藏詳細資料重建縮圖。`, "需要處理", "查看藏書", () => {
        setActivityPanelOpen(false);
        location.hash = state.libraryRouteHash;
      }));
    }
    if (state.thumbnailCacheJob) {
      const job = state.thumbnailCacheJob;
      const running = job.status === "running";
      const hasErrors = job.status === "completed_with_errors";
      const eta = running ? formatThumbnailEta(job.estimated_seconds_remaining) : hasErrors ? `${formatNumber(job.failed)} 張失敗` : "全部完成";
      ui.activityList.append(activityItem(
        `thumbnail-cache ${running ? "running" : hasErrors ? "failed" : "succeeded"}`,
        running ? "正在建立快取縮圖" : `最近一次快取縮圖 #${job.id}`,
        `${formatNumber(job.ready + job.failed)} / ${formatNumber(job.total)} · ${formatProgressPercent(job.progress_percent)} · ${eta}`,
        running ? "進行中" : hasErrors ? "部分完成" : "完成",
        hasErrors ? `查看 ${formatNumber(job.failed)} 本失敗收藏` : "查看設定",
        hasErrors ? openThumbnailCacheFailures : () => {
          setActivityPanelOpen(false);
          location.hash = "settings";
        },
        hasErrors ? "重試失敗項目" : null,
        hasErrors ? retryThumbnailCacheFailures : null,
      ));
    }
    if (state.batchRunning) {
      const batch = state.batchRunning;
      ui.activityList.append(activityItem("batch running", batch.title, `已完成 ${formatNumber(batch.completed)} / ${formatNumber(batch.total)}；已完成項目不會回滾。`, "進行中", "查看工作台", () => {
        setActivityPanelOpen(false);
        location.hash = "workbench";
      }));
    }
    if (state.lastBatchActivity) {
      const batch = state.lastBatchActivity;
      ui.activityList.append(activityItem(`batch ${batch.failed ? "failed" : "succeeded"}`, batch.title, `${batch.summary} · ${formatMetadataTime(batch.updatedAt)}`, batch.failed ? "部分完成" : "完成", "查看工作台", () => {
        setActivityPanelOpen(false);
        location.hash = "workbench";
      }));
    }
    ui.activityEmpty.hidden = ui.activityList.children.length > 0;

    const signature = [state.serviceOnline, runningCount, attentionCount, state.activityScan?.status || "", state.thumbnailCacheJob ? `${state.thumbnailCacheJob.id}:${state.thumbnailCacheJob.status}:${state.thumbnailCacheJob.progress_percent}` : "", state.batchRunning ? `${state.batchRunning.title}:${state.batchRunning.completed}:${state.batchRunning.total}` : "", state.externalBatch ? `${state.externalBatch.id}:${state.externalBatch.summary.pending}:${state.externalBatch.summary.running}:${state.externalBatch.summary.partial}:${state.externalBatch.summary.failed}` : "", ...activeJobs.map((job) => `${job.id}:${job.status}`), ...failedJobs.map((job) => `${job.id}:${job.status}`)].join("|");
    if (state.activitySignature != null && signature !== state.activitySignature) {
      ui.activityAnnouncer.textContent = summary;
    }
    state.activitySignature = signature;
  }

  function activityItem(className, title, detail, status, actionLabel, action, secondaryActionLabel = null, secondaryAction = null) {
    const item = el("li", `activity-item ${className}`);
    const copy = el("div", "");
    copy.append(el("strong", "", title), el("p", "", detail));
    const badge = el("span", "activity-status", status);
    const button = el("button", "text-button", actionLabel);
    button.type = "button";
    button.addEventListener("click", action);
    const actions = el("div", "activity-actions");
    actions.append(button);
    if (secondaryActionLabel && secondaryAction) {
      const secondary = el("button", "text-button activity-secondary-action", secondaryActionLabel);
      secondary.type = "button";
      secondary.addEventListener("click", secondaryAction);
      actions.append(secondary);
    }
    item.append(copy, badge, actions);
    return item;
  }

  function scanActivity(scan) {
    const issues = Array.isArray(scan.issues) ? scan.issues : [];
    const summary = scan.summary || null;
    const issueCount = issues.length;
    let message = scan.error_message || "掃描已完成。";
    if (summary) {
      message = `新增 ${formatNumber(summary.added || 0)}、略過 ${formatNumber(summary.skipped || 0)}、問題 ${formatNumber(issueCount)}`;
    }
    return {
      id: Number(scan.id || scan.scan_run_id) || null,
      status: scan.status || "failed",
      summary,
      issues,
      errorMessage: scan.error_message || null,
      message,
      updatedAt: scan.completed_at || scan.updatedAt || new Date().toISOString(),
    };
  }

  async function showScanResults() {
    setActivityPanelOpen(false);
    let scan = state.activityScan;
    if (!scan) return;
    ui.scanResultsSummary.textContent = "正在讀取掃描結果…";
    ui.scanIssueList.replaceChildren();
    ui.scanResultsEmpty.hidden = true;
    ui.scanResultsDialog.showModal();
    if (scan.id) {
      try {
        scan = scanActivity(await api(`/api/scans/${scan.id}`));
        state.activityScan = scan;
        renderActivityCenter();
      } catch (error) {
        ui.scanResultsSummary.textContent = `無法讀取掃描 #${scan.id}：${error.message}`;
        ui.scanResultsEmpty.hidden = false;
        return;
      }
    }
    renderScanResults(scan);
  }

  function renderScanResults(scan) {
    const issues = scan.issues || [];
    const differences = scan.summary?.preflight_differences || [];
    const status = scan.status === "partial" ? "部分完成" : scan.status === "failed" ? "失敗" : "完成";
    ui.scanResultsSummary.textContent = `掃描${scan.id ? ` #${scan.id}` : ""}${status}；共 ${formatNumber(issues.length)} 個逐筆問題${differences.length ? `，${formatNumber(differences.length)} 項與預覽不同` : ""}。${scan.errorMessage ? ` ${scan.errorMessage}` : ""}`;
    ui.scanIssueList.replaceChildren();
    ui.scanResultsEmpty.hidden = issues.length !== 0 || differences.length !== 0;
    differences.forEach((difference) => {
      const item = el("li", "scan-issue-item");
      const heading = el("div", "scan-issue-heading");
      heading.append(el("strong", "", "與預覽不同"), el("code", "", "preflight_drift"));
      item.append(heading, el("p", "", difference));
      ui.scanIssueList.append(item);
    });
    issues.forEach((issue) => {
      const item = el("li", "scan-issue-item");
      const heading = el("div", "scan-issue-heading");
      heading.append(
        el("strong", "", SCAN_ISSUE_KIND_LABELS[issue.kind] || issue.kind || "掃描問題"),
        el("code", "", issue.kind || "unknown"),
      );
      const path = el("code", "scan-issue-path", issue.path || "未指定路徑");
      const message = el("p", "", issue.message || "沒有提供錯誤訊息");
      const copy = el("button", "text-button", "複製路徑");
      copy.type = "button";
      copy.disabled = !issue.path;
      copy.addEventListener("click", () => copyScanIssuePath(issue.path, copy));
      item.append(heading, path, message, copy);
      ui.scanIssueList.append(item);
    });
  }

  async function copyScanIssuePath(path, button) {
    if (!path) return;
    try {
      await navigator.clipboard.writeText(path);
      const original = button.textContent;
      button.textContent = "已複製";
      window.setTimeout(() => { button.textContent = original; }, 1600);
    } catch (_) {
      toast("無法存取剪貼簿；請手動選取路徑", true);
    }
  }

  async function openActivityCollection(collectionId) {
    try {
      const collection = await api(`/api/collections/${collectionId}`);
      setActivityPanelOpen(false);
      await navigateToCollection(collection);
    } catch (error) {
      toast(`無法開啟這筆收藏：${error.message}`, true);
    }
  }

  async function loadShelf() {
    if (state.shelfLoaded) return;
    ui.shelfLoading.hidden = false;
    ui.shelfContent.hidden = true;
    try {
      const [stats, recent, downloads, candidateData, savedViews] = await Promise.all([
        api("/api/stats"),
        shelfCollectionPage(),
        shelfCollectionPage({ source: "downloads" }, 1),
        api("/api/tombstone-candidates"),
        loadSavedViews(),
      ]);
      const featuredName = stats.top_parody?.[0]?.name || null;
      const quickParodyName = stats.top_parody?.find((entry) => entry.name !== "オリジナル")?.name || featuredName;
      const eventName = stats.top_event?.[0]?.name || null;
      const featured = featuredName ? await shelfCollectionPage({ parody: featuredName }) : recent;
      const eventShelf = eventName ? await shelfCollectionPage({ event: eventName }) : recent;

      state.statsData = stats;
      state.candidates = candidateData.items || [];
      state.workbenchLoaded = true;
      state.shelfData = { recent, featured, eventShelf, featuredName, eventName };
      state.shelfLoaded = true;

      const pending = state.candidates.filter((candidate) => candidate.decision === "pending");
      const pendingGroups = new Set(pending.map((candidate) => candidate.tombstone_collection_id)).size;
      byId("shelf-tidy-summary").textContent = `${formatNumber(stats.missing_metadata)} 冊缺 metadata · ${formatNumber(pendingGroups)} 組同名待裁決 · ${formatNumber(downloads.pagination.total)} 冊新收藏`;
      byId("shelf-footer-status").textContent = `${formatNumber(stats.total)} 冊已編目 · 本機服務正常`;

      byId("recent-shelf-count").textContent = `${formatNumber(recent.items.length)} 冊`;
      byId("featured-shelf-heading").textContent = featuredName || "編目精選";
      byId("featured-shelf-count").textContent = `${formatNumber(featured.pagination.total)} 冊`;
      byId("event-shelf-heading").textContent = eventName || "場次書架";
      byId("event-shelf-count").textContent = `${formatNumber(eventShelf.pagination.total)} 冊`;

      if (eventName) {
        const eventQuickFilter = byId("shelf-quick-event");
        eventQuickFilter.textContent = eventName;
        eventQuickFilter.dataset.value = eventName;
      }
      if (quickParodyName) {
        const parodyQuickFilter = byId("shelf-quick-parody");
        parodyQuickFilter.textContent = quickParodyName;
        parodyQuickFilter.dataset.value = quickParodyName;
      }

      const thumbnailRequestEpoch = nextThumbnailRequestEpoch();
      renderShelfBooks(ui.recentShelfBooks, recent, null, null, false, thumbnailRequestEpoch);
      renderShelfBooks(ui.featuredShelfBooks, featured, "parody", featuredName, true, thumbnailRequestEpoch);
      renderShelfBooks(ui.eventShelfBooks, eventShelf, "event", eventName, false, thumbnailRequestEpoch);
      renderSavedViewShelf(savedViews);
      renderTombstoneCandidates();
      updateWorkbenchBadge();
      ui.shelfContent.hidden = false;
    } catch (error) {
      ui.shelfLoading.replaceChildren(el("strong", "", "無法整理書架"), document.createTextNode(`：${error.message}`));
      toast(error.message, true);
      return;
    } finally {
      if (state.shelfLoaded) ui.shelfLoading.hidden = true;
    }
  }

  async function loadSavedViews({ force = false } = {}) {
    if (state.savedViewsLoaded && !force) return state.savedViews;
    if (state.savedViewsPromise) return state.savedViewsPromise;
    state.savedViewsPromise = api("/api/saved-views")
      .then((data) => {
        state.savedViews = Array.isArray(data.items) ? data.items : [];
        state.savedViewsLoaded = true;
        restoreSavedViewLayout();
        renderSavedViewContext();
        if (state.shelfLoaded) renderSavedViewShelf(state.savedViews);
        return state.savedViews;
      })
      .catch((error) => {
        toast(`無法讀取 Saved Views：${error.message}`, true);
        return state.savedViews;
      })
      .finally(() => {
        state.savedViewsPromise = null;
      });
    return state.savedViewsPromise;
  }

  function restoreSavedViewLayout() {
    const view = activeSavedView();
    if (!view) return;
    const current = currentSavedViewQuery();
    current.layout = view.query.layout;
    if (savedViewQueryKey(current) === savedViewQueryKey(view.query)) {
      setLayout(view.query.layout);
    }
  }

  function renderSavedViewShelf(views = state.savedViews) {
    if (!ui.savedViewList) return;
    ui.savedViewList.replaceChildren();
    const pinned = views.filter((view) => view.pinned).slice(0, SAVED_VIEW_SHELF_LIMIT);
    if (!pinned.length) {
      const empty = el("li", "saved-view-empty");
      empty.append(
        el("strong", "", "還沒有釘選的智慧書架"),
        el("span", "", "在「全部藏書」組好條件後，儲存目前檢視。"),
      );
      ui.savedViewList.append(empty);
      return;
    }
    pinned.forEach((view) => {
      const item = el("li", "saved-view-item");
      const button = el("button", "saved-view-button");
      button.type = "button";
      button.addEventListener("click", () => openSavedView(view));
      const heading = el("span", "saved-view-button-heading");
      heading.append(el("strong", "", view.name), el("b", "", formatNumber(view.result_count)));
      const summary = savedViewSummary(view.query).filter((part) => !part.startsWith("排列：")).slice(0, 3).join(" · ");
      button.append(heading, el("small", "", summary || "全部藏書"));
      item.append(button);
      ui.savedViewList.append(item);
    });
  }

  function openSavedView(view) {
    state.activeSavedViewId = view.id;
    setAppliedFilters(savedViewFilters(view.query));
    state.sort = view.query.sort;
    state.direction = view.query.direction;
    ui.librarySort.value = `${state.sort}:${state.direction}`;
    setLayout(view.query.layout);
    state.libraryFocusId = null;
    renderSavedViewContext();
    navigateLibrary();
  }

  function savedViewFilters(query) {
    const filters = {};
    if (query.q) filters.q = query.q;
    ["source", "classification", "event", "circle", "author", "parody", "subcategory"].forEach((name) => {
      if (query[name]) filters[name] = query[name];
    });
    if (Array.isArray(query.tag) && query.tag.length) filters.tag = [...query.tag];
    if (Array.isArray(query.missing) && query.missing.length) filters.missing = query.missing[0];
    if (query.untagged) filters.untagged = "1";
    return filters;
  }

  function currentSavedViewQuery() {
    const filters = state.filters;
    return {
      q: filters.q || null,
      source: filters.source || null,
      classification: filters.classification || null,
      missing: filters.missing ? (Array.isArray(filters.missing) ? [...filters.missing] : [filters.missing]) : [],
      event: filters.event || null,
      circle: filters.circle || null,
      author: filters.author || null,
      parody: filters.parody || null,
      subcategory: filters.subcategory || null,
      tag: Array.isArray(filters.tag) ? [...filters.tag] : [],
      untagged: filters.untagged === "1" || filters.untagged === true,
      sort: state.sort,
      direction: state.direction,
      layout: state.layout,
    };
  }

  function savedViewQueryKey(query) {
    return JSON.stringify({
      q: query.q || null,
      source: query.source || null,
      classification: query.classification || null,
      missing: Array.isArray(query.missing) ? query.missing : [],
      event: query.event || null,
      circle: query.circle || null,
      author: query.author || null,
      parody: query.parody || null,
      subcategory: query.subcategory || null,
      tag: Array.isArray(query.tag) ? query.tag : [],
      untagged: Boolean(query.untagged),
      sort: query.sort || "created",
      direction: query.direction || "desc",
      layout: query.layout === "list" ? "list" : "grid",
    });
  }

  function activeSavedView() {
    return state.savedViews.find((view) => view.id === state.activeSavedViewId) || null;
  }

  function savedViewIsModified(view = activeSavedView()) {
    return Boolean(view && savedViewQueryKey(currentSavedViewQuery()) !== savedViewQueryKey(view.query));
  }

  function renderSavedViewContext() {
    if (!ui.savedViewContext) return;
    const view = activeSavedView();
    const active = Boolean(view);
    ui.savedViewContext.hidden = !active;
    ui.saveCurrentView.hidden = active;
    ui.updateSavedView.hidden = !active;
    ui.saveAsView.hidden = !active;
    ui.renameSavedView.hidden = !active;
    ui.deleteSavedView.hidden = !active;
    if (!view) return;
    const modified = savedViewIsModified(view);
    ui.savedViewActiveName.textContent = view.name;
    ui.savedViewDirty.hidden = !modified;
    ui.updateSavedView.disabled = !modified;
    ui.updateSavedView.title = modified ? "以目前條件明確覆寫這個 Saved View" : "目前條件與保存規則相同";
  }

  function savedViewSummary(query) {
    const parts = [];
    if (query.q) parts.push(`搜尋「${query.q}」`);
    const labels = {
      source: "來源",
      classification: "種類",
      event: "場次",
      circle: "社團",
      author: "作者",
      parody: "原作",
      subcategory: "子分類",
    };
    Object.entries(labels).forEach(([name, label]) => {
      if (query[name]) parts.push(`${label}：${query[name]}`);
    });
    (query.missing || []).forEach((value) => parts.push(value === "any" ? "缺少 metadata" : `缺少：${METADATA_LABELS[value] || value}`));
    if (query.untagged) parts.push("尚無標籤");
    if (query.tag?.length) parts.push(`標籤同時包含：${query.tag.join(" ＋ ")}`);
    const sortLabels = {
      "created:desc": "最近加入",
      "created:asc": "最早加入",
      "updated:desc": "最近修改",
      "updated:asc": "最久未修改",
      "title:asc": "標題 A → Z",
      "title:desc": "標題 Z → A",
    };
    parts.push(`排序：${sortLabels[`${query.sort}:${query.direction}`] || "最近加入"}`);
    parts.push(`排列：${query.layout === "list" ? "條列" : "書牆"}`);
    return parts;
  }

  function renderSavedViewRuleSummary(query) {
    ui.savedViewRuleSummary.replaceChildren();
    const heading = el("strong", "", "將保存的條件");
    const list = el("ul", "");
    savedViewSummary(query).forEach((part) => list.append(el("li", "", part)));
    ui.savedViewRuleSummary.append(heading, list);
  }

  function openSavedViewDialog(mode) {
    const view = activeSavedView();
    if ((mode === "rename" || mode === "save-as") && !view) return;
    state.savedViewDialogMode = mode;
    const isRename = mode === "rename";
    const query = isRename ? view.query : currentSavedViewQuery();
    ui.savedViewDialogHeading.textContent = isRename
      ? "重新命名 Saved View"
      : mode === "save-as" ? "另存新檢視" : "儲存目前檢視";
    ui.savedViewDialogIntro.textContent = isRename
      ? "只變更名稱與書架釘選狀態；即使目前 Library 條件已修改，也不會覆寫保存規則。"
      : "保存的是目前查詢規則；新收藏與 metadata 變更會自動反映。";
    ui.savedViewName.value = isRename ? view.name : mode === "save-as" ? `${view.name} 副本` : "";
    ui.savedViewForm.elements.pinned.checked = isRename ? view.pinned : true;
    ui.confirmSavedView.textContent = isRename ? "儲存名稱" : "儲存檢視";
    renderSavedViewRuleSummary(query);
    ui.savedViewDialog.showModal();
    ui.savedViewName.focus();
    ui.savedViewName.select();
  }

  async function submitSavedViewDialog(event) {
    event.preventDefault();
    const mode = state.savedViewDialogMode;
    const view = activeSavedView();
    const isRename = mode === "rename";
    if (isRename && !view) return;
    const name = ui.savedViewName.value.trim();
    if (!name) return;
    const body = {
      name,
      pinned: ui.savedViewForm.elements.pinned.checked,
      query: isRename ? view.query : currentSavedViewQuery(),
    };
    ui.confirmSavedView.disabled = true;
    try {
      const saved = await api(isRename ? `/api/saved-views/${view.id}` : "/api/saved-views", {
        method: isRename ? "PUT" : "POST",
        body,
      });
      upsertSavedView(saved);
      if (!isRename) {
        state.activeSavedViewId = saved.id;
        navigateLibrary({ replace: true });
      }
      ui.savedViewDialog.close();
      renderSavedViewContext();
      toast(isRename ? "Saved View 已重新命名" : "目前檢視已保存到 catalog");
    } catch (error) {
      toast(error.message, true);
    } finally {
      ui.confirmSavedView.disabled = false;
    }
  }

  async function updateActiveSavedView() {
    const view = activeSavedView();
    if (!view || !savedViewIsModified(view)) return;
    ui.updateSavedView.disabled = true;
    try {
      const saved = await api(`/api/saved-views/${view.id}`, {
        method: "PUT",
        body: { name: view.name, pinned: view.pinned, query: currentSavedViewQuery() },
      });
      upsertSavedView(saved);
      renderSavedViewContext();
      toast(`已更新「${saved.name}」的保存規則`);
    } catch (error) {
      toast(error.message, true);
      renderSavedViewContext();
    }
  }

  async function deleteActiveSavedView() {
    const view = activeSavedView();
    if (!view || !window.confirm(`刪除 Saved View「${view.name}」？收藏本身不會被刪除。`)) return;
    try {
      await api(`/api/saved-views/${view.id}`, { method: "DELETE" });
      state.savedViews = state.savedViews.filter((entry) => entry.id !== view.id);
      state.activeSavedViewId = null;
      navigateLibrary({ replace: true });
      renderSavedViewContext();
      if (state.shelfLoaded) renderSavedViewShelf();
      toast("Saved View 已刪除；收藏資料未變更");
    } catch (error) {
      toast(error.message, true);
    }
  }

  function upsertSavedView(saved) {
    state.savedViews = [saved, ...state.savedViews.filter((view) => view.id !== saved.id)]
      .sort((left, right) => Number(right.pinned) - Number(left.pinned) || right.updated_at.localeCompare(left.updated_at) || right.id - left.id);
    state.savedViewsLoaded = true;
    if (state.shelfLoaded) renderSavedViewShelf();
  }

  function shelfCollectionPage(filters = {}, perPage = SHELF_LIMIT) {
    const params = new URLSearchParams({ page: "1", per_page: String(perPage) });
    Object.entries(filters).forEach(([name, value]) => {
      if (value) params.set(name, value);
    });
    return api(`/api/collections?${params}`);
  }

  function renderShelfBooks(container, page, filterName, filterValue, featured, thumbnailRequestEpoch) {
    unbindThumbnailsWithin(container);
    container.replaceChildren();
    const books = (page.items || []).slice(0, 7);
    if (!books.length) {
      container.append(el("li", "shelf-empty", "這座書架目前沒有收藏。"));
      updateShelfScrollControls(container.closest(".shelf-scroll-shell"));
      return;
    }
    books.forEach((collection) => {
      const item = el("li", "shelf-book");
      const button = el("button", "shelf-book-button");
      button.type = "button";
      button.setAttribute("aria-label", `在全部藏書檢視 ${displayTitle(collection)}`);
      button.addEventListener("click", () => openShelfBook(collection, filterName, filterValue));
      const cover = document.createElement("img");
      cover.className = "shelf-cover";
      cover.alt = "";
      cover.width = 179;
      cover.height = 239;
      cover.loading = "lazy";
      bindThumbnail(cover, collection.id, thumbnailRequestEpoch);
      const kicker = el("span", "shelf-book-kicker", collection.circle || collection.authors?.[0] || "未標示社團");
      const title = el("strong", "", displayTitle(collection));
      const meta = el("small", "", [collection.event, collection.parody || collection.parody_raw].filter(Boolean).join(" · ") || "尚待整理");
      button.append(cover, kicker, title, meta);
      item.append(button);
      container.append(item);
    });
    const remaining = Math.max(0, Number(page.pagination?.total || 0) - books.length);
    if (remaining > 0) {
      const more = el("li", "shelf-more");
      const button = el("button", featured ? "shelf-more-button featured" : "shelf-more-button", `+ ${formatNumber(remaining)} 冊 →`);
      button.type = "button";
      button.addEventListener("click", () => showShelfFilter(filterName, filterValue));
      more.append(button);
      container.append(more);
    }
    requestAnimationFrame(() => updateShelfScrollControls(container.closest(".shelf-scroll-shell")));
  }

  function initializeShelfScrollControls() {
    document.querySelectorAll(".shelf-scroll-shell").forEach((shell) => {
      const scroller = shell.querySelector(".shelf-books");
      shell.querySelectorAll("[data-shelf-scroll]").forEach((button) => {
        button.addEventListener("click", () => {
          const direction = button.dataset.shelfScroll === "previous" ? -1 : 1;
          const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
          scroller.scrollBy({ left: direction * Math.max(193, scroller.clientWidth * 0.82), behavior: reducedMotion ? "auto" : "smooth" });
        });
      });
      scroller.addEventListener("scroll", () => updateShelfScrollControls(shell), { passive: true });
      updateShelfScrollControls(shell);
    });
    window.addEventListener("resize", () => document.querySelectorAll(".shelf-scroll-shell").forEach(updateShelfScrollControls), { passive: true });
  }

  function updateShelfScrollControls(shell) {
    if (!shell) return;
    const scroller = shell.querySelector(".shelf-books");
    const canScrollLeft = scroller.scrollLeft > 2;
    const canScrollRight = scroller.scrollLeft + scroller.clientWidth < scroller.scrollWidth - 2;
    shell.classList.toggle("can-scroll-left", canScrollLeft);
    shell.classList.toggle("can-scroll-right", canScrollRight);
    const previous = shell.querySelector('[data-shelf-scroll="previous"]');
    const next = shell.querySelector('[data-shelf-scroll="next"]');
    previous.hidden = !canScrollLeft;
    next.hidden = !canScrollRight;
  }

  async function openShelfBook(collection, filterName, filterValue) {
    state.activeSavedViewId = null;
    setAppliedFilters(filterName && filterValue ? { [filterName]: filterValue } : {});
    await navigateToCollection(collection);
  }

  function showShelfFilter(name, value) {
    state.activeSavedViewId = null;
    setAppliedFilters(name && value ? { [name]: value } : {});
    navigateLibrary();
  }

  function collectFilterDraft() {
    const data = new FormData(ui.searchForm);
    const filters = {};
    const query = String(data.get("q") || "").trim();
    if (query) filters.q = query;
    FILTER_NAMES.forEach((name) => {
      if (name === "tag") return;
      const value = String(data.get(name) || "").trim();
      if (value) filters[name] = value;
    });
    if (state.filterTags.length) filters.tag = [...state.filterTags];
    return filters;
  }

  function cloneFilters(filters) {
    return Object.fromEntries(Object.entries(filters).map(([name, value]) => [name, Array.isArray(value) ? [...value] : value]));
  }

  function syncFilterDraftFromApplied() {
    ui.searchForm.reset();
    Object.entries(state.filters).forEach(([name, value]) => {
      if (name === "tag") return;
      const control = ui.searchForm.elements[name];
      if (control) control.value = value;
    });
    state.filterTags = Array.isArray(state.filters.tag) ? [...state.filters.tag] : [];
    renderFilterTagChips();
    updateFilterDraftState();
  }

  function filterDraftChanged() {
    return libraryParams(collectFilterDraft(), null).toString() !== libraryParams(state.filters, null).toString();
  }

  function updateFilterDraftState() {
    const changed = filterDraftChanged();
    ui.filterDraftStatus.hidden = !changed;
    ui.applyFilters.disabled = !changed;
    ui.filterToggle.classList.toggle("has-draft", changed);
  }

  function setAppliedFilters(filters) {
    state.filters = cloneFilters(filters);
    state.libraryFocusId = null;
    syncFilterDraftFromApplied();
    updateFilterCount();
    renderSavedViewContext();
  }

  function applyFilterDraft() {
    state.filters = collectFilterDraft();
    state.libraryFocusId = null;
    updateFilterCount();
    updateFilterDraftState();
    renderSavedViewContext();
    setFilterPanelOpen(false);
    navigateLibrary();
  }

  function applyHeaderSearch() {
    const keepCurrent = state.route === "library" && ui.headerSearchScope.value === "current";
    if (!keepCurrent) state.activeSavedViewId = null;
    const filters = keepCurrent ? cloneFilters(state.filters) : {};
    const query = ui.searchInput.value.trim();
    if (query) filters.q = query;
    else delete filters.q;
    setAppliedFilters(filters);
    if (!ui.filterPanel.hidden) setFilterPanelOpen(false);
    navigateLibrary();
  }

  function updateFilterCount() {
    const count = appliedFilterCount();
    ui.activeFilterCount.textContent = String(count);
    ui.filterToggle.setAttribute("aria-label", count ? `更多篩選，目前套用 ${count} 項條件` : "更多篩選，目前沒有套用條件");
    renderActiveFilterChips();
  }

  function renderActiveFilterChips() {
    if (!ui.activeFilterChips) return;
    ui.activeFilterChips.replaceChildren();
    Object.entries(state.filters).forEach(([name, value]) => {
      if (Array.isArray(value)) {
        value.forEach((entry) => appendActiveFilterChip(name, entry));
        return;
      }
      appendActiveFilterChip(name, value);
    });
  }

  function appendActiveFilterChip(name, value) {
    const chip = el("button", "active-filter-chip");
    chip.type = "button";
    chip.title = `移除${FILTER_LABELS[name] || name}篩選`;
    const standaloneLabel = name === "untagged" || name === "missing" && value === "any";
    const displayValue = name === "untagged" ? "尚無標籤" : standaloneLabel ? "缺少 metadata" : value;
    chip.append(document.createTextNode(`${standaloneLabel ? displayValue : `${FILTER_LABELS[name] || name}：${displayValue}`} `), el("span", "", "×"));
    chip.addEventListener("click", () => removeFilter(name, value));
    ui.activeFilterChips.append(chip);
  }

  function removeFilter(name, value = null) {
    const filters = cloneFilters(state.filters);
    if (name === "tag") {
      filters.tag = (filters.tag || []).filter((tag) => tag !== value);
      if (!filters.tag.length) delete filters.tag;
    } else {
      delete filters[name];
    }
    setAppliedFilters(filters);
    navigateLibrary();
  }

  function clearAppliedFilters() {
    setAppliedFilters({});
    setFilterPanelOpen(false);
    navigateLibrary();
  }

  function changeLibrarySort() {
    const [sort, direction] = ui.librarySort.value.split(":");
    state.sort = ["created", "updated", "title"].includes(sort) ? sort : "created";
    state.direction = ["asc", "desc"].includes(direction) ? direction : "desc";
    state.libraryFocusId = null;
    renderSavedViewContext();
    navigateLibrary();
  }

  function initializeFacetComboboxes() {
    ui.filterPanel.querySelectorAll("[data-facet-field]").forEach((fieldElement) => {
      const field = fieldElement.dataset.facetField;
      const input = fieldElement.querySelector('[role="combobox"]');
      const listbox = fieldElement.querySelector('[role="listbox"]');
      const controller = { field, input, listbox, options: [], activeIndex: -1, requestNumber: 0, timer: null };
      facetControllers.set(field, controller);
      input.addEventListener("focus", () => queueFacetSearch(controller, 0));
      input.addEventListener("input", () => queueFacetSearch(controller, 140));
      input.addEventListener("blur", () => setTimeout(() => closeFacetOptions(controller), 160));
      input.addEventListener("keydown", (event) => handleFacetKeydown(event, controller));
    });
    renderFilterTagChips();
  }

  function queueFacetSearch(controller, delay) {
    clearTimeout(controller.timer);
    controller.timer = setTimeout(() => loadFacetOptions(controller), delay);
  }

  async function loadFacetOptions(controller) {
    const requestNumber = ++controller.requestNumber;
    const params = new URLSearchParams({ field: controller.field, q: controller.input.value.trim(), limit: "20" });
    try {
      const data = await api(`/api/facets?${params}`);
      if (requestNumber !== controller.requestNumber) return;
      const selectedTags = new Set(state.filterTags.map((tag) => tag.toLocaleLowerCase()));
      controller.options = (data.items || []).filter(
        (option) => controller.field !== "tag" || !selectedTags.has(option.name.toLocaleLowerCase()),
      );
      renderFacetOptions(controller);
    } catch (_) {
      if (requestNumber === controller.requestNumber) closeFacetOptions(controller);
    }
  }

  function renderFacetOptions(controller) {
    controller.listbox.replaceChildren();
    controller.activeIndex = -1;
    controller.input.removeAttribute("aria-activedescendant");
    if (!controller.options.length) {
      controller.listbox.append(el("li", "facet-empty", "沒有符合的選項"));
    } else {
      controller.options.forEach((option, index) => {
        const item = el("li", "facet-option");
        item.id = `facet-${controller.field}-option-${index}`;
        item.setAttribute("role", "option");
        item.setAttribute("aria-selected", "false");
        item.setAttribute("aria-label", option.name);
        item.append(el("span", "", option.name), el("small", "", formatNumber(option.count)));
        item.addEventListener("click", () => selectFacetOption(controller, index));
        item.addEventListener("pointermove", (event) => {
          if (event.pointerType === "mouse") setFacetActive(controller, index);
        });
        controller.listbox.append(item);
      });
    }
    controller.listbox.hidden = false;
    controller.input.setAttribute("aria-expanded", "true");
  }

  function handleFacetKeydown(event, controller) {
    if (event.key === "Escape" && !controller.listbox.hidden) {
      event.preventDefault();
      event.stopPropagation();
      closeFacetOptions(controller);
      return;
    }
    if (["ArrowDown", "ArrowUp"].includes(event.key)) {
      event.preventDefault();
      if (controller.listbox.hidden || !controller.options.length) {
        queueFacetSearch(controller, 0);
        return;
      }
      const direction = event.key === "ArrowDown" ? 1 : -1;
      const start = controller.activeIndex < 0 ? (direction > 0 ? -1 : 0) : controller.activeIndex;
      setFacetActive(controller, (start + direction + controller.options.length) % controller.options.length);
      return;
    }
    if (event.key !== "Enter") return;
    if (!controller.listbox.hidden && controller.activeIndex >= 0) {
      event.preventDefault();
      selectFacetOption(controller, controller.activeIndex);
    } else if (controller.field === "tag" && controller.input.value.trim()) {
      event.preventDefault();
      addFilterTag(controller.input.value);
      controller.input.value = "";
      closeFacetOptions(controller);
    }
  }

  function setFacetActive(controller, index) {
    controller.activeIndex = index;
    controller.listbox.querySelectorAll('[role="option"]').forEach((option, optionIndex) => {
      const active = optionIndex === index;
      option.classList.toggle("is-active", active);
      option.setAttribute("aria-selected", String(active));
    });
    const activeOption = controller.listbox.querySelectorAll('[role="option"]')[index];
    if (!activeOption) return;
    controller.input.setAttribute("aria-activedescendant", activeOption.id);
    activeOption.scrollIntoView({ block: "nearest" });
  }

  function selectFacetOption(controller, index) {
    const option = controller.options[index];
    if (!option) return;
    if (controller.field === "tag") {
      addFilterTag(option.name);
      controller.input.value = "";
    } else {
      controller.input.value = option.name;
      updateFilterDraftState();
    }
    closeFacetOptions(controller);
    controller.input.focus({ preventScroll: true });
  }

  function closeFacetOptions(controller) {
    clearTimeout(controller.timer);
    controller.listbox.hidden = true;
    controller.activeIndex = -1;
    controller.input.setAttribute("aria-expanded", "false");
    controller.input.removeAttribute("aria-activedescendant");
  }

  function closeAllFacetOptions() {
    facetControllers.forEach(closeFacetOptions);
  }

  function addFilterTag(value) {
    const tag = String(value || "").trim();
    if (!tag || state.filterTags.some((existing) => existing.toLocaleLowerCase() === tag.toLocaleLowerCase())) return;
    state.filterTags.push(tag);
    renderFilterTagChips();
    updateFilterDraftState();
  }

  function removeFilterTag(value) {
    state.filterTags = state.filterTags.filter((tag) => tag !== value);
    renderFilterTagChips();
    updateFilterDraftState();
  }

  function renderFilterTagChips() {
    if (!ui.filterTagChips) return;
    ui.filterTagChips.replaceChildren();
    state.filterTags.forEach((tag) => {
      const chip = el("button", "filter-tag-chip");
      chip.type = "button";
      chip.setAttribute("aria-label", `移除標籤篩選 ${tag}`);
      chip.append(document.createTextNode(tag), el("span", "", "×"));
      chip.addEventListener("click", () => removeFilterTag(tag));
      ui.filterTagChips.append(chip);
    });
  }

  function initializeTagSuggestionInputs() {
    document.querySelectorAll("[data-tag-suggestions]").forEach((container) => {
      const input = container.querySelector('[role="combobox"]');
      const listbox = container.querySelector('[role="listbox"]');
      const controller = { input, listbox, form: input.closest("form"), options: [], activeIndex: -1, requestNumber: 0, timer: null };
      tagSuggestionControllers.set(input, controller);
      input.addEventListener("focus", () => queueTagSuggestions(controller, 0));
      input.addEventListener("input", () => queueTagSuggestions(controller, 140));
      input.addEventListener("blur", () => setTimeout(() => closeTagSuggestions(controller), 160));
      input.addEventListener("keydown", (event) => handleTagSuggestionKeydown(event, controller));
    });
  }

  function queueTagSuggestions(controller, delay) {
    clearTimeout(controller.timer);
    controller.timer = setTimeout(() => loadTagSuggestions(controller), delay);
  }

  async function loadTagSuggestions(controller) {
    const requestNumber = ++controller.requestNumber;
    const params = new URLSearchParams({ field: "tag", q: controller.input.value.trim(), limit: "20" });
    try {
      const data = await api(`/api/facets?${params}`);
      if (requestNumber !== controller.requestNumber) return;
      controller.options = data.items || [];
      renderTagSuggestions(controller);
    } catch (_) {
      if (requestNumber === controller.requestNumber) closeTagSuggestions(controller);
    }
  }

  function renderTagSuggestions(controller) {
    controller.listbox.replaceChildren();
    controller.activeIndex = -1;
    controller.input.removeAttribute("aria-activedescendant");
    if (!controller.options.length) {
      controller.listbox.append(el("li", "facet-empty", "沒有既有標籤；按 Enter 建立新標籤"));
    } else {
      controller.options.forEach((option, index) => {
        const item = el("li", "facet-option");
        item.id = `${controller.listbox.id}-option-${index}`;
        item.setAttribute("role", "option");
        item.setAttribute("aria-selected", "false");
        item.setAttribute("aria-label", `${option.name}，使用 ${formatNumber(option.count)} 次`);
        item.append(el("span", "", option.name), el("small", "", `${formatNumber(option.count)} 次`));
        item.addEventListener("click", () => selectTagSuggestion(controller, index, true));
        item.addEventListener("pointermove", (event) => {
          if (event.pointerType === "mouse") setTagSuggestionActive(controller, index);
        });
        controller.listbox.append(item);
      });
    }
    controller.listbox.hidden = false;
    controller.input.setAttribute("aria-expanded", "true");
  }

  function handleTagSuggestionKeydown(event, controller) {
    if (event.key === "Escape" && !controller.listbox.hidden) {
      event.preventDefault();
      event.stopPropagation();
      closeTagSuggestions(controller);
      return;
    }
    if (["ArrowDown", "ArrowUp"].includes(event.key)) {
      event.preventDefault();
      if (controller.listbox.hidden || !controller.options.length) {
        queueTagSuggestions(controller, 0);
        return;
      }
      const direction = event.key === "ArrowDown" ? 1 : -1;
      const start = controller.activeIndex < 0 ? (direction > 0 ? -1 : 0) : controller.activeIndex;
      setTagSuggestionActive(controller, (start + direction + controller.options.length) % controller.options.length);
      return;
    }
    if (event.key !== "Enter") return;
    event.preventDefault();
    if (!controller.listbox.hidden && controller.activeIndex >= 0) {
      selectTagSuggestion(controller, controller.activeIndex, true);
    } else if (controller.input.value.trim()) {
      closeTagSuggestions(controller);
      controller.form?.requestSubmit();
    }
  }

  function setTagSuggestionActive(controller, index) {
    controller.activeIndex = index;
    controller.listbox.querySelectorAll('[role="option"]').forEach((option, optionIndex) => {
      const active = optionIndex === index;
      option.classList.toggle("is-active", active);
      option.setAttribute("aria-selected", String(active));
    });
    const activeOption = controller.listbox.querySelectorAll('[role="option"]')[index];
    if (!activeOption) return;
    controller.input.setAttribute("aria-activedescendant", activeOption.id);
    activeOption.scrollIntoView({ block: "nearest" });
  }

  function selectTagSuggestion(controller, index, submit) {
    const option = controller.options[index];
    if (!option) return;
    controller.input.value = option.name;
    closeTagSuggestions(controller);
    controller.input.focus({ preventScroll: true });
    if (submit) controller.form?.requestSubmit();
  }

  function closeTagSuggestions(controller) {
    clearTimeout(controller.timer);
    controller.listbox.hidden = true;
    controller.activeIndex = -1;
    controller.input.setAttribute("aria-expanded", "false");
    controller.input.removeAttribute("aria-activedescendant");
  }

  function closeTagSuggestionsFor(input) {
    const controller = tagSuggestionControllers.get(input);
    if (controller) closeTagSuggestions(controller);
  }

  function collectionPageParams(page) {
    const params = new URLSearchParams({
      page: String(page),
      per_page: String(PER_PAGE),
      sort: state.sort,
      direction: state.direction,
    });
    Object.entries(state.filters).forEach(([name, value]) => {
      if (Array.isArray(value)) value.forEach((entry) => params.append(name, entry));
      else params.set(name, value);
    });
    return params;
  }

  async function loadCollections({ preserveSelection = false, restoreThroughPage = 1 } = {}) {
    if (!preserveSelection) clearSelection();
    state.page = 1;
    state.totalPages = 0;
    state.total = 0;
    state.items = [];
    collectionWindowStart = -1;
    collectionWindowEnd = -1;
    state.libraryLoaded = false;
    state.libraryLoadError = false;
    state.libraryLoading = true;
    state.libraryEmptyContext = null;
    const requestNumber = ++state.requestNumber;
    ui.loading.hidden = false;
    ui.empty.hidden = true;
    ui.results.hidden = true;
    renderLibraryLoadState();
    const params = collectionPageParams(state.page);
    try {
      const data = await api(`/api/collections?${params}`);
      if (requestNumber !== state.requestNumber) return;
      state.items = data.items;
      state.total = data.pagination.total;
      state.totalPages = data.pagination.total_pages;
      if (!state.items.length) {
        state.libraryEmptyContext = await resolveLibraryEmptyContext(data, requestNumber);
        if (requestNumber !== state.requestNumber) return false;
      }
      state.libraryLoaded = true;
      ui.loading.hidden = true;
      const focusNeedsLocator = state.libraryFocusId && !state.items.some((item) => item.id === state.libraryFocusId);
      const deferFocus = restoreThroughPage > 1 || focusNeedsLocator;
      renderCollections({ deferFocus });
      if (preserveSelection) refreshLoadedSelectionRecords();
      let targetPage = restoreThroughPage;
      if (focusNeedsLocator) {
        const location = await locateLibraryFocus(state.libraryFocusId, requestNumber);
        if (location?.status === "in_query") targetPage = Math.max(targetPage, Number(location.page) || 1);
      }
      state.libraryLoading = false;
      renderLibraryLoadState();
      await restoreLibraryLoadedWindow(targetPage);
      if (deferFocus) resolveLibraryFocus();
      if (state.route === "library" && state.restoreLibraryContext) restoreLibraryWorkContext();
      setServiceState("online", "本機服務正常");
      return true;
    } catch (error) {
      if (requestNumber !== state.requestNumber) return false;
      ui.loading.hidden = true;
      ui.results.hidden = false;
      ui.results.replaceChildren();
      ui.resultSummary.textContent = "無法讀取收藏";
      renderLibraryLoadState();
      setServiceState("offline", "要求失敗");
      toast(error.message, true);
      return false;
    } finally {
      if (requestNumber === state.requestNumber) {
        state.libraryLoading = false;
        renderLibraryLoadState();
        scheduleLibraryLoadCheck();
      }
    }
  }

  async function resolveLibraryEmptyContext(queryData, requestNumber) {
    const hasFilters = Object.keys(state.filters).length > 0;
    try {
      const [rootData, unfilteredData] = await Promise.all([
        api("/api/library-roots"),
        hasFilters
          ? api(`/api/collections?${new URLSearchParams({ page: "1", per_page: "1" })}`)
          : Promise.resolve(queryData),
      ]);
      if (requestNumber !== state.requestNumber) return null;
      const roots = rootData.roots || [];
      if (!roots.length) return { kind: "no_roots", filterCount: appliedFilterCount() };
      if (!roots.some((root) => root.active)) return { kind: "inactive_roots", filterCount: appliedFilterCount() };
      if (Number(unfilteredData.pagination?.total) === 0) return { kind: "needs_scan", filterCount: appliedFilterCount() };
      return { kind: "query", filterCount: appliedFilterCount() };
    } catch (_) {
      return { kind: "query", filterCount: appliedFilterCount() };
    }
  }

  async function locateLibraryFocus(focusId, requestNumber) {
    try {
      const params = collectionPageParams(1);
      const location = await api(`/api/collections/${focusId}/locate?${params}`);
      if (requestNumber !== state.requestNumber || state.route !== "library" || state.libraryFocusId !== focusId) return null;
      if (location.status === "not_in_query") presentOutOfQueryFocus(location.collection);
      return location;
    } catch (error) {
      if (requestNumber !== state.requestNumber || state.route !== "library" || state.libraryFocusId !== focusId) return null;
      state.libraryFocusId = null;
      navigateLibrary({ replace: true });
      toast(`無法定位這筆收藏：${error.message}`, true);
      return null;
    }
  }

  function presentOutOfQueryFocus(collection) {
    state.outOfQueryCollection = collection;
    state.libraryFocusId = null;
    navigateLibrary({ replace: true });
    ui.focusFilterMessage.textContent = `「${displayTitle(collection)}」不符合目前的搜尋或篩選。你可以保留目前結果，或清除條件後定位這筆收藏。`;
    if (!ui.focusFilterDialog.open) ui.focusFilterDialog.showModal();
  }

  async function clearFiltersAndFocus() {
    const collection = state.outOfQueryCollection;
    if (!collection) return;
    ui.focusFilterDialog.close();
    state.outOfQueryCollection = null;
    setAppliedFilters({});
    await navigateToCollection(collection);
  }

  async function restoreLibraryLoadedWindow(targetPage) {
    const finalPage = Math.min(Math.max(1, Number(targetPage) || 1), state.totalPages);
    while (state.page < finalPage) {
      if (!await loadMoreCollections()) break;
    }
  }

  function refreshLoadedSelectionRecords() {
    state.items.forEach((collection) => {
      if (state.selectedIds.has(collection.id)) state.selectedRecords.set(collection.id, collection);
    });
  }

  function initializeLibraryInfiniteScroll() {
    window.addEventListener("scroll", scheduleCollectionWindowUpdate, { passive: true });
    window.addEventListener("resize", () => {
      collectionRowHeights.grid = 0;
      collectionRowHeights.list = 0;
      scheduleCollectionWindowUpdate(true);
    }, { passive: true });
    if (typeof window.IntersectionObserver !== "function") {
      window.addEventListener("scroll", scheduleLibraryLoadCheck, { passive: true });
      window.addEventListener("resize", scheduleLibraryLoadCheck, { passive: true });
      return;
    }
    libraryScrollObserver = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) loadMoreCollections();
    }, { rootMargin: "1200px 0px" });
    libraryScrollObserver.observe(ui.libraryScrollSentinel);
  }

  function scheduleLibraryLoadCheck() {
    window.requestAnimationFrame(() => {
      if (state.route !== "library" || !state.libraryLoaded || state.libraryLoading || state.libraryLoadError) return;
      const bounds = ui.libraryScrollSentinel.getBoundingClientRect();
      if (bounds.top <= window.innerHeight + 1200) loadMoreCollections();
    });
  }

  async function loadMoreCollections() {
    if (libraryLoadPromise) return libraryLoadPromise;
    if (state.route !== "library" || !state.libraryLoaded || state.libraryLoading || state.page >= state.totalPages) return false;
    libraryLoadPromise = requestNextCollectionPage();
    try {
      return await libraryLoadPromise;
    } finally {
      libraryLoadPromise = null;
    }
  }

  async function requestNextCollectionPage() {
    const nextPage = state.page + 1;
    const requestNumber = ++state.requestNumber;
    state.libraryLoading = true;
    state.libraryLoadError = false;
    renderLibraryLoadState();
    const params = collectionPageParams(nextPage);
    try {
      const data = await api(`/api/collections?${params}`);
      if (requestNumber !== state.requestNumber) return false;
      const existingIds = new Set(state.items.map((item) => item.id));
      const additions = data.items.filter((item) => !existingIds.has(item.id));
      const startIndex = state.items.length;
      state.items.push(...additions);
      state.page = data.pagination.page;
      state.total = data.pagination.total;
      state.totalPages = data.pagination.total_pages;
      refreshLoadedSelectionRecords();
      if (additions.length) renderCollectionWindow({ anchorIndex: startIndex });
      updateLibrarySummary();
      updateSelectionUI();
      if (additions.length) {
        const remaining = Math.max(0, state.total - state.items.length);
        ui.libraryLoadAnnouncer.textContent = remaining > 0
          ? `已載入 ${formatNumber(additions.length)} 筆，尚有 ${formatNumber(remaining)} 筆`
          : `已載入 ${formatNumber(additions.length)} 筆，已顯示全部 ${formatNumber(state.total)} 筆`;
      }
      setServiceState("online", "本機服務正常");
      return true;
    } catch (error) {
      if (requestNumber !== state.requestNumber) return false;
      state.libraryLoadError = true;
      setServiceState("offline", "要求失敗");
      ui.libraryLoadAnnouncer.textContent = "更多收藏載入失敗，可使用重試載入";
      toast(`無法載入更多收藏：${error.message}`, true);
      return false;
    } finally {
      if (requestNumber === state.requestNumber) {
        state.libraryLoading = false;
        renderLibraryLoadState();
        scheduleLibraryLoadCheck();
      }
    }
  }

  function renderCollections({ deferFocus = false } = {}) {
    ui.results.hidden = state.items.length === 0;
    ui.empty.hidden = state.items.length !== 0;
    if (!state.items.length) renderLibraryEmptyState();
    updateLibrarySummary();
    renderCollectionWindow({ anchorIndex: state.items.findIndex((item) => item.id === state.libraryFocusId) });

    if (!deferFocus) resolveLibraryFocus();
    updateSelectionUI();
  }

  function renderLibraryEmptyState() {
    const context = state.libraryEmptyContext || { kind: "query", filterCount: appliedFilterCount() };
    const views = {
      no_roots: {
        symbol: "源",
        heading: "先加入你的藏書資料夾",
        description: "登記新收藏或典藏庫所在的資料夾後，即可建立本機索引。",
        primary: "前往設定新增來源",
      },
      inactive_roots: {
        symbol: "停",
        heading: "目前沒有啟用中的資料夾來源",
        description: "重新啟用至少一個資料夾來源後，才能繼續建立或更新收藏索引。",
        primary: "管理資料夾來源",
      },
      needs_scan: {
        symbol: "掃",
        heading: "資料夾已設定，尚未建立收藏索引",
        description: "執行首次掃描後，這裡會顯示資料夾中的收藏。掃描只會處理已登記且啟用的來源。",
        primary: "開始首次掃描",
        secondary: "查看資料夾來源",
      },
      query: {
        symbol: "空",
        heading: "沒有符合條件的收藏",
        description: "試著縮短關鍵字，或移除一項搜尋與篩選條件。",
        primary: "清除搜尋與篩選",
      },
    };
    const view = views[context.kind] || views.query;
    ui.emptySymbol.textContent = view.symbol;
    ui.emptyHeading.textContent = view.heading;
    ui.emptyDescription.textContent = view.description;
    ui.emptyPrimary.textContent = view.primary;
    ui.emptyPrimary.disabled = false;
    ui.emptySecondary.hidden = !view.secondary;
    ui.emptySecondary.textContent = view.secondary || "";
    const showFilterContext = context.kind === "query" && context.filterCount > 0;
    ui.emptyContext.hidden = !showFilterContext;
    ui.emptyContext.textContent = showFilterContext
      ? `目前套用 ${formatNumber(context.filterCount)} 項搜尋或篩選條件；上方條件標籤可逐項移除。`
      : "";
  }

  function appliedFilterCount() {
    return Object.values(state.filters).reduce(
      (total, value) => total + (Array.isArray(value) ? value.length : value ? 1 : 0),
      0,
    );
  }

  function handleLibraryEmptyPrimary() {
    const kind = state.libraryEmptyContext?.kind || "query";
    if (kind === "no_roots") {
      openLibraryRootSettings("new");
      return;
    }
    if (kind === "inactive_roots") {
      openLibraryRootSettings("manage");
      return;
    }
    if (kind === "needs_scan") {
      scanEmptyLibrary();
      return;
    }
    clearAppliedFilters();
  }

  function handleLibraryEmptySecondary() {
    if (state.libraryEmptyContext?.kind === "needs_scan") openLibraryRootSettings("manage");
  }

  function openLibraryRootSettings(mode) {
    state.settingsRootFocus = mode;
    location.hash = "settings";
  }

  function resolveLibraryFocus() {
    if (!applyLibraryFocus()) {
      if (state.items[0] && !window.matchMedia("(max-width: 899px)").matches) selectCollection(state.items[0]);
      else {
        state.libraryFocusId = null;
        clearDetail();
      }
    }
  }

  function renderCollectionWindow({ anchorIndex = null, force = false } = {}) {
    cancelCollectionWindowUpdate();
    if (!ui.results || !state.items.length) {
      unbindThumbnailsWithin(ui.results);
      ui.results?.replaceChildren();
      collectionWindowStart = 0;
      collectionWindowEnd = 0;
      return;
    }
    const columns = collectionColumnCount();
    const anchor = Number.isInteger(anchorIndex) && anchorIndex >= 0
      ? anchorIndex
      : estimatedVisibleCollectionIndex();
    const { start, end } = collectionWindowRange(state.items.length, anchor, columns);
    if (!force && start === collectionWindowStart && end === collectionWindowEnd && ui.results.querySelector(".collection-item")) {
      updateCollectionSpacers();
      return;
    }
    unbindThumbnailsWithin(ui.results);
    ui.results.replaceChildren();
    collectionWindowStart = start;
    collectionWindowEnd = end;
    const topSpacer = collectionSpacer("top");
    const bottomSpacer = collectionSpacer("bottom");
    if (start > 0) ui.results.append(topSpacer);
    appendCollectionItems(state.items.slice(start, end), start);
    if (end < state.items.length) ui.results.append(bottomSpacer);
    updateCollectionSpacers();
    window.requestAnimationFrame(() => {
      measureCollectionRowHeight();
      updateCollectionSpacers();
    });
  }

  function collectionWindowRange(total, anchorIndex, columns) {
    const alignedWindowSize = Math.max(columns, Math.floor(COLLECTION_WINDOW_SIZE / columns) * columns);
    const maximumStart = Math.max(0, Math.floor((total - alignedWindowSize) / columns) * columns);
    const centered = Math.max(0, anchorIndex - Math.floor(alignedWindowSize / 2));
    const start = Math.min(maximumStart, Math.floor(centered / columns) * columns);
    const end = total - start <= COLLECTION_WINDOW_SIZE
      ? total
      : Math.min(total, start + alignedWindowSize);
    return { start, end };
  }

  function collectionColumnCount() {
    if (state.layout === "list") return 1;
    const template = window.getComputedStyle(ui.results).gridTemplateColumns;
    return Math.max(1, template.split(" ").filter(Boolean).length);
  }

  function collectionSpacer(position) {
    const spacer = el("li", "collection-window-spacer");
    spacer.dataset.windowSpacer = position;
    spacer.setAttribute("aria-hidden", "true");
    return spacer;
  }

  function updateCollectionSpacers() {
    const rowHeight = collectionRowHeights[state.layout] || (state.layout === "list" ? 82 : 320);
    const columns = collectionColumnCount();
    const gap = Number.parseFloat(window.getComputedStyle(ui.results).rowGap) || 0;
    const topRows = Math.ceil(collectionWindowStart / columns);
    const bottomRows = Math.ceil((state.items.length - collectionWindowEnd) / columns);
    const topSpacer = ui.results.querySelector('[data-window-spacer="top"]');
    const bottomSpacer = ui.results.querySelector('[data-window-spacer="bottom"]');
    if (topSpacer) topSpacer.style.height = `${Math.max(0, topRows * rowHeight - gap)}px`;
    if (bottomSpacer) bottomSpacer.style.height = `${Math.max(0, bottomRows * rowHeight - gap)}px`;
  }

  function measureCollectionRowHeight() {
    const items = ui.results.querySelectorAll(".collection-item");
    const columns = collectionColumnCount();
    if (!items.length) return;
    const first = items[0].getBoundingClientRect();
    const lastRowIndex = Math.floor((items.length - 1) / columns) * columns;
    const lastRow = items[lastRowIndex]?.getBoundingClientRect();
    const gap = Number.parseFloat(window.getComputedStyle(ui.results).rowGap) || 0;
    const rowDistance = lastRowIndex / columns;
    const measured = rowDistance > 0 ? (lastRow.top - first.top) / rowDistance : first.height + gap;
    if (measured > 0) collectionRowHeights[state.layout] = measured;
  }

  function estimatedVisibleCollectionIndex() {
    if (!state.items.length) return 0;
    const rowHeight = collectionRowHeights[state.layout] || (state.layout === "list" ? 82 : 320);
    const columns = collectionColumnCount();
    const resultsTop = ui.results.getBoundingClientRect().top + window.scrollY;
    const row = Math.max(0, Math.floor((window.scrollY - resultsTop) / rowHeight));
    return Math.min(state.items.length - 1, row * columns);
  }

  function scheduleCollectionWindowUpdate(force = false) {
    if (collectionWindowFrame != null) return;
    collectionWindowFrame = window.requestAnimationFrame(() => {
      collectionWindowFrame = null;
      if (state.route !== "library" || !state.libraryLoaded || state.items.length <= COLLECTION_WINDOW_SIZE) return;
      const anchor = estimatedVisibleCollectionIndex();
      const nearStart = anchor < collectionWindowStart + COLLECTION_WINDOW_OVERSCAN;
      const nearEnd = anchor >= collectionWindowEnd - COLLECTION_WINDOW_OVERSCAN;
      if (force || nearStart || nearEnd) renderCollectionWindow({ anchorIndex: anchor, force });
    });
  }

  function cancelCollectionWindowUpdate() {
    if (collectionWindowFrame == null) return;
    window.cancelAnimationFrame(collectionWindowFrame);
    collectionWindowFrame = null;
  }

  function ensureCollectionMounted(index) {
    if (index < collectionWindowStart || index >= collectionWindowEnd) {
      renderCollectionWindow({ anchorIndex: index });
    }
  }

  function appendCollectionItems(collections, startIndex, container = ui.results) {
    const thumbnailRequestEpoch = nextThumbnailRequestEpoch();
    collections.forEach((collection, offset) => {
      const item = el("li", "collection-item");
      const selection = document.createElement("input");
      selection.type = "checkbox";
      selection.className = "collection-checkbox";
      selection.dataset.collectionTitle = displayTitle(collection);
      updateSelectionCheckbox(selection, state.selectedIds.has(collection.id));
      selection.addEventListener("change", () => {
        updateSelectionCheckbox(selection);
        toggleCollectionSelection(collection, selection.checked);
      });
      const selectionControl = el("label", "collection-select-control");
      selectionControl.addEventListener("click", (event) => event.stopPropagation());
      const selectionMark = el("span", "selection-mark", "✓");
      selectionMark.setAttribute("aria-hidden", "true");
      selectionControl.append(selection, selectionMark);
      const button = el("button", "collection-item-button");
      button.type = "button";
      button.dataset.collectionId = String(collection.id);
      button.setAttribute("aria-current", String(state.selected?.id === collection.id));
      button.setAttribute("aria-label", `查看 ${displayTitle(collection)} 詳情`);
      button.addEventListener("click", () => {
        const scrollPosition = window.scrollY;
        selectCollection(collection);
        openMobileDetail(button, scrollPosition);
      });

      const cover = document.createElement("img");
      cover.className = "item-cover";
      cover.alt = "";
      cover.width = 179;
      cover.height = 239;
      cover.loading = "lazy";
      bindThumbnail(cover, collection.id, thumbnailRequestEpoch);

      const copy = el("span", "item-copy");
      const creator = collection.circle || collection.authors?.[0] || "未標示社團";
      const kicker = el("span", "item-kicker", creator);
      const title = el("span", "item-title", displayTitle(collection));
      const meta = el("span", "item-meta", [collection.event, collection.parody || collection.parody_raw].filter(Boolean).join(" · ") || "場次與原作未整理");
      const flags = el("span", "item-flags");
      [collection.event, collection.classification_top, collection.parody].filter(Boolean).slice(0, 3).forEach((value) => {
        flags.append(el("span", "mini-flag", value));
      });
      copy.append(kicker, title, meta, flags);
      const index = el("span", "item-index", String(startIndex + offset + 1).padStart(4, "0"));
      button.append(cover, copy, index);
      item.append(selectionControl, button);
      container.append(item);
    });
  }

  function renderLibraryLoadState() {
    if (!state.libraryLoaded || state.total === 0) {
      ui.loadMore.hidden = true;
      ui.loadMore.setAttribute("aria-busy", "false");
      return;
    }
    ui.loadMore.hidden = false;
    ui.loadMore.setAttribute("aria-busy", String(state.libraryLoading));
    if (state.libraryLoading) {
      ui.loadMoreSpinner.hidden = false;
      ui.retryLibraryLoad.hidden = true;
      ui.loadMoreLabel.textContent = `正在載入更多收藏…已載入 ${formatNumber(state.items.length)} 筆`;
      return;
    }
    ui.loadMoreSpinner.hidden = true;
    if (state.libraryLoadError) {
      ui.retryLibraryLoad.hidden = false;
      ui.retryLibraryLoad.textContent = "重試載入";
      ui.loadMoreLabel.textContent = "更多收藏載入失敗";
      return;
    }
    const allLoaded = state.page >= state.totalPages;
    ui.retryLibraryLoad.hidden = allLoaded;
    ui.retryLibraryLoad.textContent = "載入更多";
    ui.loadMoreLabel.textContent = allLoaded
      ? `已顯示全部 ${formatNumber(state.total)} 筆收藏`
      : `已載入 ${formatNumber(state.items.length)} / ${formatNumber(state.total)} 筆收藏`;
  }

  function setLayout(layout) {
    const focusedIndex = state.items.findIndex((item) => item.id === state.libraryFocusId);
    const anchor = focusedIndex >= 0 ? focusedIndex : estimatedVisibleCollectionIndex();
    state.layout = layout === "grid" ? "grid" : "list";
    writeStorage(LAYOUT_KEY, state.layout);
    ui.results?.classList.toggle("layout-list", state.layout === "list");
    ui.results?.classList.toggle("layout-grid", state.layout === "grid");
    document.querySelectorAll("[data-layout]").forEach((button) => {
      button.setAttribute("aria-pressed", String(button.dataset.layout === state.layout));
    });
    renderSavedViewContext();
    if (state.libraryLoaded && state.items.length) {
      renderCollectionWindow({ anchorIndex: anchor, force: true });
      if (focusedIndex >= 0) {
        window.requestAnimationFrame(() => {
          cancelCollectionWindowUpdate();
          ensureCollectionMounted(focusedIndex);
          ui.results.querySelector(`[data-collection-id="${state.libraryFocusId}"]`)?.scrollIntoView({ block: "nearest" });
        });
      }
    }
  }

  function applyLibraryFocus() {
    if (!state.libraryFocusId) return false;
    const collection = state.items.find((item) => item.id === state.libraryFocusId);
    if (collection) {
      selectCollection(collection, { updateRoute: false });
      return true;
    }
    state.libraryFocusId = null;
    ui.results.querySelector('.collection-item-button[aria-current="true"]')?.setAttribute("aria-current", "false");
    clearDetail();
    return false;
  }

  function selectCollection(collection, { focus = false, updateRoute = true } = {}) {
    cancelCollectionWindowUpdate();
    const previousId = state.selected?.id;
    const index = state.items.findIndex((item) => item.id === collection.id);
    if (index >= 0) ensureCollectionMounted(index);
    state.selected = collection;
    state.libraryFocusId = collection.id;
    if (previousId !== collection.id) {
      ui.results.querySelector(`[data-collection-id="${previousId}"]`)?.setAttribute("aria-current", "false");
    }
    const button = ui.results.querySelector(`[data-collection-id="${collection.id}"]`);
    button?.setAttribute("aria-current", "true");
    renderDetail(collection);
    if (updateRoute && state.route === "library") navigateLibrary({ replace: true });
    if (focus) {
      button?.focus({ preventScroll: true });
      button?.scrollIntoView({ block: "nearest" });
    }
  }

  function renderDetail(collection) {
    ui.detailPlaceholder.hidden = true;
    ui.collectionDetail.hidden = false;
    ui.detailCover.alt = `${displayTitle(collection)}的封面`;
    bindThumbnail(ui.detailCover, collection.id);
    ui.detailSource.textContent = collection.root?.source === "downloads" ? "新收藏" : "典藏庫";
    ui.detailKicker.textContent = [collection.event, collection.classification_top, collection.classification_subcategory].filter(Boolean).join(" · ") || "尚未分類";
    ui.detailTitle.textContent = displayTitle(collection);
    ui.detailFilename.textContent = collection.filename;
    ui.detailPath.textContent = collection.path;

    ui.metadataList.replaceChildren();
    const rows = [
      ["社團", metadataValues(collection.circle, "circle")],
      ["作者", (collection.authors || []).map((value) => ({ value, filter: "author" }))],
      ["原作", metadataValues(collection.parody || collection.parody_raw, "parody")],
      ["場次", metadataValues(collection.event, "event")],
      ["種類", metadataValues([collection.classification_top, collection.classification_subcategory].filter(Boolean).join("／"), "classification", collection.classification_top)],
      ["版本", metadataValues(collection.is_dl == null ? null : collection.is_dl ? "DL 版" : "非 DL 版")],
      ["來源", metadataValues(collection.root?.label)],
    ];
    rows.forEach(([label, values]) => {
      const term = el("dt", "", label);
      const description = el("dd", values.length ? "" : "metadata-missing");
      if (!values.length) {
        description.textContent = "尚無資料";
      } else {
        values.forEach((entry, index) => {
          if (index > 0) description.append(document.createTextNode("、"));
          if (!entry.filter) {
            description.append(document.createTextNode(entry.value));
            return;
          }
          const filter = el("button", "metadata-filter", entry.value);
          filter.type = "button";
          filter.title = `以${label}「${entry.filterValue || entry.value}」篩選`;
          filter.addEventListener("click", () => applyFilter(entry.filter, entry.filterValue || entry.value));
          description.append(filter);
        });
      }
      ui.metadataList.append(term, description);
    });
    renderTags(collection);
    renderDataQualitySummary();
    prepareMetadataEvidence(collection);
  }

  function missingMetadataFields(collection) {
    if (!collection) return [];
    return [
      ["title", "標題", collection.title],
      ["circle", "社團", collection.circle],
      ["authors", "作者", collection.authors?.length],
      ["parody", "原作", collection.parody || collection.parody_raw],
      ["event", "場次", collection.event],
      ["classification", "種類", collection.classification_top],
      ["is_dl", "版本", collection.is_dl != null],
    ].filter(([, , value]) => !value).map(([field, label]) => ({ field, label }));
  }

  function renderMissingMetadataActions(missing) {
    ui.missingMetadataActions.replaceChildren();
    ui.missingMetadataActions.hidden = missing.length === 0;
    missing.forEach(({ field, label }) => {
      const button = el("button", "text-button", `補上${label}`);
      button.type = "button";
      button.addEventListener("click", () => openMetadataDialog(field));
      ui.missingMetadataActions.append(button);
    });
  }

  function renderDataQualitySummary() {
    if (!ui.dataQualitySummary || !ui.evidenceSummaryCount) return;
    const missing = missingMetadataFields(state.selected);
    const missingLabels = missing.map(({ label }) => label);
    renderMissingMetadataActions(missing);
    const assertions = (state.metadataHistory?.fields || []).flatMap((field) => field.assertions || []);
    const pending = assertions.filter((assertion) => assertion.status === "candidate").length;
    const externalStatus = state.externalJob?.status;
    const thumbnailFailed = ui.detailCover?.dataset.thumbnailStatus === "failed";
    const parts = [];
    if (missing.length) parts.push(`缺少 ${missing.length} 欄（${missingLabels.join("、")}）`);
    if (pending) parts.push(`${pending} 筆 assertion 待裁決`);
    if (["pending", "running"].includes(externalStatus)) parts.push("外部搜尋進行中");
    if (externalStatus === "partial") parts.push("外部搜尋部分完成");
    if (externalStatus === "failed") parts.push("外部搜尋失敗");
    if (thumbnailFailed) parts.push("縮圖失敗");

    const attentionCount = missing.length + pending + Number(["partial", "failed"].includes(externalStatus)) + Number(thumbnailFailed);
    const checking = !state.metadataHistory && state.metadataHistoryCollectionId === state.selected?.id;
    ui.metadataEvidence.classList.toggle("has-data-quality-issues", attentionCount > 0);
    ui.metadataEvidence.classList.toggle("has-data-quality-work", ["pending", "running"].includes(externalStatus));
    if (parts.length) {
      ui.dataQualitySummary.textContent = `${parts.join(" · ")}。展開可查看來源與處理工具。`;
      ui.evidenceSummaryCount.textContent = attentionCount > 0 ? `待處理 ${attentionCount}` : "處理中";
    } else if (checking) {
      ui.dataQualitySummary.textContent = "正在檢查編目資料與來源紀錄…";
      ui.evidenceSummaryCount.textContent = "檢查中";
    } else {
      ui.dataQualitySummary.textContent = "目前沒有待處理問題；展開可檢視完整來源與歷史。";
      ui.evidenceSummaryCount.textContent = "狀態良好";
    }
  }

  function openMobileDetail(originButton, scrollPosition = window.scrollY) {
    if (!mobileDetailMedia.matches || !state.selected || ui.mobileDetailDialog.open) return;
    mobileDetailReturnId = originButton?.dataset.collectionId || String(state.selected.id);
    mobileDetailScrollPosition = scrollPosition;
    mobileDetailRestoreFocus = true;
    ui.mobileDetailContent.append(ui.collectionDetail);
    document.body.style.setProperty("--mobile-detail-scroll-offset", `-${mobileDetailScrollPosition}px`);
    document.body.classList.add("mobile-detail-open");
    ui.mobileDetailDialog.showModal();
    ui.mobileDetailClose.focus({ preventScroll: true });
  }

  function closeMobileDetail({ restoreFocus = true } = {}) {
    if (!ui.mobileDetailDialog?.open) return;
    mobileDetailRestoreFocus = restoreFocus;
    ui.mobileDetailDialog.close();
  }

  function finishMobileDetailClose() {
    if (ui.collectionDetail.parentElement !== ui.detailPane) ui.detailPane.append(ui.collectionDetail);
    const returnId = mobileDetailReturnId;
    const restoreFocus = mobileDetailRestoreFocus;
    const scrollPosition = mobileDetailScrollPosition;
    mobileDetailReturnId = null;
    mobileDetailRestoreFocus = true;
    document.body.classList.remove("mobile-detail-open");
    document.body.style.removeProperty("--mobile-detail-scroll-offset");
    const restoreContext = restoreFocus && state.route === "library" && mobileDetailMedia.matches;
    if (restoreContext) window.scrollTo({ top: scrollPosition, behavior: "auto" });
    requestAnimationFrame(() => {
      if (restoreFocus && returnId) {
        document.querySelector(`[data-collection-id="${returnId}"]`)?.focus({ preventScroll: true });
      }
      if (restoreContext) window.scrollTo({ top: scrollPosition, behavior: "auto" });
    });
  }

  function clearDetail() {
    closeMobileDetail({ restoreFocus: false });
    state.selected = null;
    resetMetadataEvidence();
    unbindThumbnail(ui.detailCover);
    ui.collectionDetail.hidden = true;
    ui.detailPlaceholder.hidden = false;
  }

  function metadataValues(value, filter = null, filterValue = null) {
    return value ? [{ value, filter, filterValue }] : [];
  }

  function applyFilter(name, value) {
    if (name !== "tag" && !FILTER_NAMES.includes(name)) return;
    const filters = cloneFilters(state.filters);
    if (name === "tag") {
      const tags = Array.isArray(filters.tag) ? filters.tag : [];
      if (!tags.some((tag) => tag.toLocaleLowerCase() === value.toLocaleLowerCase())) filters.tag = [...tags, value];
    } else filters[name] = value;
    closeMobileDetail({ restoreFocus: false });
    setAppliedFilters(filters);
    navigateLibrary();
    toast(`已加入篩選：${value}`);
  }

  function updateLibrarySummary() {
    if (!ui.resultSummary) return;
    ui.resultSummary.textContent = `批次選取 ${formatNumber(state.selectedIds.size)} / 已載入 ${formatNumber(state.items.length)} / 符合 ${formatNumber(state.total)}`;
  }

  function renderTags(collection) {
    ui.detailTags.replaceChildren();
    if (!collection.tags?.length) {
      ui.detailTags.append(el("span", "tag-empty", "尚未加入標籤"));
      return;
    }
    collection.tags.forEach((tag) => {
      const chip = el("span", "tag-chip");
      chip.append(document.createTextNode(tag));
      const remove = el("button", "", "×");
      remove.type = "button";
      remove.setAttribute("aria-label", `移除標籤 ${tag}`);
      remove.addEventListener("click", () => removeTag(tag));
      chip.append(remove);
      ui.detailTags.append(chip);
    });
  }

  async function launchSelected(kind) {
    if (!state.selected) return;
    const button = kind === "read" ? byId("read-button") : byId("open-button");
    const original = button.textContent;
    button.disabled = true;
    button.textContent = kind === "read" ? "正在啟動閱讀器…" : "正在開啟…";
    try {
      await api(`/api/collections/${state.selected.id}/${kind}`, { method: "POST" });
      rememberLaunch(state.selected, kind);
      toast(kind === "read" ? "已交給閱讀器開啟" : "已交給系統開啟");
    } catch (error) {
      toast(error.message, true);
    } finally {
      button.disabled = false;
      button.textContent = original;
    }
  }

  function rememberLaunch(collection, action) {
    const entry = {
      id: collection.id,
      title: displayTitle(collection),
      filename: collection.filename,
      action,
      openedAt: new Date().toISOString(),
    };
    state.recent = [entry, ...state.recent.filter((item) => item.id !== entry.id)].slice(0, RECENT_LIMIT);
    writeStorage(RECENT_KEY, state.recent);
    renderRecent();
  }

  function renderRecent() {
    if (!ui.recentList) return;
    ui.recentCount.textContent = String(state.recent.length);
    ui.recentList.replaceChildren();
    if (state.recent.length === 0) {
      ui.recentList.append(el("li", "recent-empty", "成功開啟收藏後，紀錄會出現在這裡。"));
      byId("clear-recent").hidden = true;
      return;
    }
    byId("clear-recent").hidden = false;
    state.recent.forEach((entry) => {
      const item = el("li", "recent-item");
      const button = el("button", "", entry.title);
      button.type = "button";
      button.append(el("small", "", `${entry.action === "read" ? "閱讀器" : "系統開啟"} · ${entry.filename}`));
      button.addEventListener("click", () => openRecent(entry.id));
      const time = document.createElement("time");
      time.dateTime = entry.openedAt;
      time.textContent = formatRecentTime(entry.openedAt);
      item.append(button, time);
      ui.recentList.append(item);
    });
  }

  async function openRecent(id) {
    try {
      const collection = await api(`/api/collections/${id}`);
      ui.recentDialog.close();
      await navigateToCollection(collection);
    } catch (error) {
      toast(`無法載入這筆最近紀錄：${error.message}`, true);
    }
  }

  function clearRecent() {
    if (!window.confirm("清除這個瀏覽器中的最近開啟紀錄？收藏資料不會受到影響。")) return;
    state.recent = [];
    writeStorage(RECENT_KEY, state.recent);
    renderRecent();
    toast("已清除最近開啟紀錄");
  }

  async function addTag(event) {
    event.preventDefault();
    if (!state.selected) return;
    const name = ui.tagInput.value.trim();
    if (!name) {
      toast("請輸入要新增的標籤", true);
      return;
    }
    try {
      const collection = await api(`/api/collections/${state.selected.id}/tags`, {
        method: "POST",
        body: { name },
      });
      ui.tagInput.value = "";
      closeTagSuggestionsFor(ui.tagInput);
      replaceSelected(collection);
      invalidateDerivedData();
      toast(`已加入標籤「${name}」`);
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function removeTag(name) {
    if (!state.selected) return;
    try {
      const collection = await api(`/api/collections/${state.selected.id}/tags`, {
        method: "DELETE",
        body: { name },
      });
      replaceSelected(collection);
      invalidateDerivedData();
      toast(`已移除標籤「${name}」`);
    } catch (error) {
      toast(error.message, true);
    }
  }

  function openMetadataDialog(field = null, collection = state.selected) {
    if (!collection) return;
    state.metadataEditCollection = collection;
    const selectedField = field && METADATA_LABELS[field] ? field : "title";
    ui.metadataField.value = selectedField;
    syncMetadataEditor();
    ui.metadataDialog.showModal();
    window.requestAnimationFrame(() => {
      if (!field) {
        ui.metadataField.focus();
        return;
      }
      const target = selectedField === "classification"
        ? ui.metadataForm.elements.classification_top
        : selectedField === "is_dl"
          ? ui.metadataForm.elements.boolean_value
          : ui.metadataValue;
      target.focus();
    });
  }

  function syncMetadataEditor() {
    const field = ui.metadataField.value;
    const collection = state.metadataEditCollection || state.selected;
    const isClassification = field === "classification";
    const isBoolean = field === "is_dl";
    ui.metadataTextGroup.hidden = isClassification || isBoolean;
    ui.metadataClassificationGroup.hidden = !isClassification;
    ui.metadataBooleanGroup.hidden = !isBoolean;
    if (!collection) return;

    if (isClassification) {
      ui.metadataForm.elements.classification_top.value = collection.classification_top || "其他";
      ui.metadataForm.elements.classification_subcategory.value = collection.classification_subcategory || "";
    } else if (isBoolean) {
      ui.metadataForm.elements.boolean_value.value = collection.is_dl === false ? "false" : "true";
    } else {
      ui.metadataValueLabel.firstChild.textContent = `${METADATA_LABELS[field]}的新值`;
      const current = field === "authors" ? collection.authors?.join("、") : field === "parody" ? collection.parody || collection.parody_raw : collection[field];
      ui.metadataValue.value = current || "";
      ui.metadataValue.placeholder = field === "authors" ? "多位作者請用逗號或頓號分隔" : "";
    }
  }

  async function saveMetadata(event) {
    event.preventDefault();
    const target = state.metadataEditCollection || state.selected;
    if (!target) return;
    const field = ui.metadataField.value;
    let value;
    if (field === "classification") {
      value = {
        top_level: ui.metadataForm.elements.classification_top.value,
        subcategory: ui.metadataForm.elements.classification_subcategory.value.trim() || null,
      };
    } else if (field === "is_dl") {
      value = ui.metadataForm.elements.boolean_value.value === "true";
    } else if (field === "authors") {
      value = ui.metadataValue.value.split(/[、,，\n]+/).map((part) => part.trim()).filter(Boolean);
    } else {
      value = ui.metadataValue.value.trim();
    }
    if (value === "" || (Array.isArray(value) && value.length === 0)) {
      toast("新值不能是空白；如要撤回手動值，請使用清除按鈕", true);
      return;
    }
    const submit = ui.metadataForm.querySelector('[type="submit"]');
    submit.disabled = true;
    try {
      const collection = await api(`/api/collections/${target.id}/metadata/${field}`, {
        method: "PUT",
        body: { value },
      });
      invalidateDerivedData();
      ui.metadataDialog.close();
      if (state.route === "review") {
        invalidateDerivedData({ library: true });
        await loadReviewQueue({ preferredId: target.id });
        const remains = state.reviewItems.some((item) => item.collection.id === target.id);
        toast(`已儲存${METADATA_LABELS[field]}的手動值${remains ? "；這本收藏仍有其他待審問題" : "；已前進下一筆"}`);
      } else {
        replaceSelected(collection);
        if (ui.metadataEvidence.open) loadMetadataEvidence(true);
        toast(`已儲存${METADATA_LABELS[field]}的手動值`);
      }
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
    }
  }

  async function clearManualMetadata() {
    const target = state.metadataEditCollection || state.selected;
    if (!target) return;
    const field = ui.metadataField.value;
    if (!window.confirm(`清除${METADATA_LABELS[field]}的手動值？系統會改用下一順位的資料。`)) return;
    try {
      const collection = await api(`/api/collections/${target.id}/metadata/${field}`, { method: "DELETE" });
      invalidateDerivedData();
      ui.metadataDialog.close();
      if (state.route === "review") {
        invalidateDerivedData({ library: true });
        await loadReviewQueue({ preferredId: target.id });
        toast(`已清除${METADATA_LABELS[field]}的手動值；Queue 已依最新狀態更新`);
      } else {
        replaceSelected(collection);
        if (ui.metadataEvidence.open) loadMetadataEvidence(true);
        toast(`已清除${METADATA_LABELS[field]}的手動值`);
      }
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function enqueueExternalSearch() {
    if (!state.selected) return;
    const collection = state.selected;
    const missing = [];
    if (!collection.title) missing.push("title");
    if (!collection.event) missing.push("event");
    if (!collection.circle) missing.push("circle");
    if (!collection.authors?.length) missing.push("authors");
    if (!collection.parody) missing.push("parody");
    if (!collection.classification_top) missing.push("classification");
    const fields = missing.length ? missing : ["title", "event", "circle", "authors", "parody", "classification"];
    const button = byId("external-search-button");
    button.disabled = true;
    try {
      const result = await api(`/api/collections/${collection.id}/external-search-jobs`, {
        method: "POST",
        body: { fields },
      });
      rememberExternalJob(result.job);
      state.externalJob = result.job;
      renderExternalJob(result.job);
      if (!ui.metadataEvidence.open) ui.metadataEvidence.open = true;
      else {
        loadMetadataEvidence(true);
        scheduleExternalJobPoll(result.job);
      }
      toast(result.created ? `已排入外部資料搜尋（${fields.map((f) => METADATA_LABELS[f]).join("、")}）` : "相同搜尋已在佇列中");
    } catch (error) {
      toast(error.message, true);
    } finally {
      button.disabled = false;
    }
  }

  function prepareMetadataEvidence(collection) {
    if (state.metadataHistoryCollectionId !== collection.id) {
      resetMetadataEvidence(collection.id);
    }
    loadMetadataEvidence();
    loadKnownExternalJob();
  }

  function resetMetadataEvidence(collectionId = null) {
    state.metadataRequestNumber += 1;
    state.metadataHistoryCollectionId = collectionId;
    state.metadataHistory = null;
    state.openMetadataFields.clear();
    state.externalJob = null;
    stopExternalJobPolling();
    ui.evidenceSummaryCount.textContent = "尚未載入";
    ui.evidenceFields.replaceChildren();
    ui.evidenceLoading.hidden = true;
    ui.evidenceError.hidden = true;
    ui.externalJobStatus.hidden = true;
    ui.externalJobStatus.replaceChildren();
    renderDataQualitySummary();
  }

  function toggleMetadataEvidence() {
    if (!ui.metadataEvidence.open) {
      return;
    }
    loadMetadataEvidence();
    loadKnownExternalJob();
  }

  async function loadMetadataEvidence(force = false) {
    const collection = state.selected;
    if (!collection || state.metadataHistoryCollectionId !== collection.id) return;
    if (!force && state.metadataHistory) {
      renderMetadataHistory(state.metadataHistory);
      return;
    }
    const requestNumber = ++state.metadataRequestNumber;
    ui.evidenceLoading.hidden = false;
    ui.evidenceError.hidden = true;
    try {
      const history = await api(`/api/collections/${collection.id}/metadata`);
      if (requestNumber !== state.metadataRequestNumber || state.selected?.id !== collection.id) return;
      state.metadataHistory = history;
      renderMetadataHistory(history);
    } catch (error) {
      if (requestNumber !== state.metadataRequestNumber || state.selected?.id !== collection.id) return;
      ui.evidenceErrorMessage.textContent = `${error.message}。請確認收藏仍是有效項目後再試一次。`;
      ui.evidenceError.hidden = false;
      ui.evidenceSummaryCount.textContent = "載入失敗";
      ui.dataQualitySummary.textContent = "無法檢查資料品質；展開後可再試一次。";
    } finally {
      if (requestNumber === state.metadataRequestNumber) ui.evidenceLoading.hidden = true;
    }
  }

  function renderMetadataHistory(history) {
    const fields = history.fields || [];
    const assertions = fields.flatMap((field) => field.assertions || []);
    const pending = assertions.filter((assertion) => assertion.status === "candidate").length;
    ui.evidenceSummaryCount.textContent = pending > 0 ? `待裁決 ${pending} · 證據 ${assertions.length}` : `證據 ${assertions.length}`;
    ui.evidenceFields.replaceChildren();
    fields.forEach((field) => ui.evidenceFields.append(metadataFieldEvidence(field)));
    renderDataQualitySummary();
  }

  function metadataFieldEvidence(field) {
    const section = el("details", "metadata-field-evidence");
    const selected = (field.assertions || []).find((assertion) => assertion.selected);
    const pending = (field.assertions || []).filter((assertion) => assertion.status === "candidate").length;
    if (pending > 0) state.openMetadataFields.add(field.field);
    section.open = state.openMetadataFields.has(field.field);
    section.addEventListener("toggle", () => {
      if (section.open) state.openMetadataFields.add(field.field);
      else state.openMetadataFields.delete(field.field);
    });

    const summary = document.createElement("summary");
    const heading = el("span", "evidence-field-heading");
    heading.append(
      el("b", "", METADATA_LABELS[field.field] || field.field),
      el("strong", selected ? "" : "metadata-missing", selected ? formatEvidenceValue(selected.value) : "未設定"),
      el("small", "", selected ? `${METADATA_SOURCE_LABELS[selected.source] || selected.source} · ${SELECTION_KIND_LABELS[field.selection?.selected_by] || field.selection?.selected_by || "未選擇"}` : "目前沒有採用值"),
    );
    const count = el("span", `evidence-field-count${pending ? " pending" : ""}`, pending ? `${pending} 待裁決` : `${(field.assertions || []).length} 筆`);
    summary.append(heading, count);
    section.append(summary);

    const body = el("div", "evidence-field-body");
    const assertionList = el("div", "assertion-list");
    if (!(field.assertions || []).length) {
      assertionList.append(el("p", "evidence-field-empty", "還沒有可供比較的 assertion。手動修正或外部搜尋後，來源會保留在這裡。"));
    } else {
      field.assertions.forEach((assertion) => assertionList.append(metadataAssertionRow(field.field, assertion)));
    }
    body.append(assertionList);
    if ((field.external_search_results || []).length) body.append(externalSearchEvidence(field.external_search_results));
    section.append(body);
    return section;
  }

  function metadataAssertionRow(field, assertion) {
    const row = el("article", `assertion-row source-${assertion.source} status-${assertion.status}${assertion.selected ? " selected" : ""}`);
    const header = el("header", "assertion-header");
    const badges = el("div", "assertion-badges");
    badges.append(
      el("span", "evidence-badge assertion-id", `#${assertion.id}`),
      el("span", `evidence-badge source-${assertion.source}`, METADATA_SOURCE_LABELS[assertion.source] || assertion.source),
      el("span", `evidence-badge status-${assertion.status}`, ASSERTION_STATUS_LABELS[assertion.status] || assertion.status),
    );
    if (assertion.selected) badges.append(el("span", "evidence-badge selected-badge", "目前採用"));
    const time = document.createElement("time");
    time.dateTime = assertion.created_at;
    time.textContent = formatMetadataTime(assertion.created_at);
    header.append(badges, time);

    const value = el("strong", "assertion-value", formatEvidenceValue(assertion.value));
    const references = el("p", "assertion-reference");
    const referenceParts = [];
    if (assertion.source_reference) referenceParts.push(assertion.source_reference);
    if (assertion.parser_run_id) referenceParts.push(`parser run #${assertion.parser_run_id}`);
    references.textContent = referenceParts.length ? referenceParts.join(" · ") : "沒有額外來源參照";
    row.append(header, value, references);
    if (assertion.reason) row.append(el("p", "assertion-reason", assertion.reason));
    if (assertion.confidence_total != null) row.append(confidenceEvidence(assertion.confidence_total, assertion.confidence));

    if (["candidate", "accepted"].includes(assertion.status)) {
      const actions = el("div", "assertion-actions");
      if (!assertion.selected) {
        const select = el("button", "secondary-button", "採用這個值");
        select.type = "button";
        select.addEventListener("click", () => decideMetadataAssertion(field, assertion, "select", select));
        actions.append(select);
      }
      const reject = el("button", "text-button assertion-reject", assertion.selected ? "拒絕並改用下一順位" : "拒絕候選");
      reject.type = "button";
      reject.addEventListener("click", () => showAssertionRejection(field, assertion, actions));
      actions.append(reject);
      row.append(actions);
    }
    return row;
  }

  function confidenceEvidence(total, confidence) {
    const wrap = el("details", "confidence-evidence");
    const summary = document.createElement("summary");
    const meter = document.createElement("meter");
    meter.min = 0;
    meter.max = 1;
    meter.value = total;
    meter.setAttribute("aria-label", `信心分數 ${formatPercent(total)}`);
    summary.append(el("span", "", `信心分數 ${formatPercent(total)}`), meter);
    wrap.append(summary);
    if (confidence && typeof confidence === "object") {
      const list = el("dl", "confidence-breakdown");
      [
        ["source_reliability", "來源可靠度"],
        ["identifier_match", "識別碼匹配"],
        ["string_similarity", "字串相似度"],
        ["rule_certainty", "規則確定度"],
      ].forEach(([key, label]) => {
        if (confidence[key] == null) return;
        list.append(el("dt", "", label), el("dd", "", formatPercent(confidence[key])));
      });
      if (confidence.reason) list.append(el("dt", "", "判斷理由"), el("dd", "", confidence.reason));
      wrap.append(list);
    }
    return wrap;
  }

  function externalSearchEvidence(results) {
    const section = el("details", "external-result-history");
    const summary = document.createElement("summary");
    summary.textContent = `外部搜尋紀錄 ${results.length} 筆`;
    section.append(summary);
    const list = el("ol", "external-result-list");
    results.forEach((result) => {
      const item = el("li", "external-result-item");
      const heading = el("div", "external-result-heading");
      heading.append(
        el("span", `evidence-badge disposition-${result.disposition}`, SEARCH_DISPOSITION_LABELS[result.disposition] || result.disposition),
        el("strong", "", formatEvidenceValue(result.value)),
      );
      item.append(heading, el("p", "", `${result.source_reference} · 信心 ${formatPercent(result.confidence_total)}`));
      item.append(el("small", "", result.assertion_id ? `已建立 assertion #${result.assertion_id}` : "僅保留搜尋證據，不能直接套用"));
      list.append(item);
    });
    section.append(list);
    return section;
  }

  function showAssertionRejection(field, assertion, actions) {
    actions.replaceChildren();
    const warning = el("div", "assertion-reject-confirm");
    warning.setAttribute("role", "alert");
    warning.append(el("p", "", "拒絕後仍會保留證據，但這筆 assertion 不能再次選取。"));
    const buttons = el("div", "assertion-reject-buttons");
    const keep = el("button", "text-button", "保留候選");
    keep.type = "button";
    keep.addEventListener("click", () => renderMetadataHistory(state.metadataHistory));
    const confirm = el("button", "danger-button", assertion.selected ? "拒絕並改用下一順位" : "確認拒絕候選");
    confirm.type = "button";
    confirm.addEventListener("click", () => decideMetadataAssertion(field, assertion, "reject", confirm));
    buttons.append(keep, confirm);
    warning.append(buttons);
    actions.append(warning);
  }

  async function decideMetadataAssertion(field, assertion, decision, button) {
    const collectionId = state.selected?.id;
    if (!collectionId) return;
    const row = button.closest(".assertion-row");
    row?.querySelectorAll("button").forEach((action) => { action.disabled = true; });
    button.textContent = decision === "select" ? "正在採用…" : "正在拒絕…";
    try {
      const history = await api(`/api/collections/${collectionId}/metadata/${field}/assertions/${assertion.id}`, {
        method: "PATCH",
        body: { decision },
      });
      if (state.selected?.id !== collectionId) return;
      state.metadataHistory = history;
      renderMetadataHistory(history);
      try {
        const collection = await api(`/api/collections/${collectionId}`);
        if (state.selected?.id === collectionId) replaceSelected(collection);
      } catch (error) {
        toast(`裁決已保存，但收藏摘要未能重新載入：${error.message}`, true);
      }
      toast(decision === "select" ? `已採用 assertion #${assertion.id}` : `已拒絕 assertion #${assertion.id}`);
    } catch (error) {
      toast(`${decision === "select" ? "無法採用" : "無法拒絕"} assertion #${assertion.id}：${error.message}`, true);
      if (state.metadataHistory) renderMetadataHistory(state.metadataHistory);
    }
  }

  function rememberExternalJob(job) {
    const key = String(job.collection_id);
    delete state.externalJobRefs[key];
    state.externalJobRefs[key] = job.id;
    const keys = Object.keys(state.externalJobRefs);
    keys.slice(0, Math.max(0, keys.length - 200)).forEach((oldKey) => delete state.externalJobRefs[oldKey]);
    writeStorage(EXTERNAL_JOB_KEY, state.externalJobRefs);
    state.activityExternalJobs.set(job.id, job);
    renderActivityCenter();
  }

  function loadKnownExternalJob() {
    const collectionId = state.selected?.id;
    if (!collectionId) return;
    const jobId = Number(state.externalJobRefs[String(collectionId)]);
    if (Number.isSafeInteger(jobId) && jobId > 0) loadExternalJob(jobId, collectionId);
  }

  async function loadExternalJob(jobId, collectionId) {
    try {
      const job = await api(`/api/external-search-jobs/${jobId}`);
      if (state.selected?.id !== collectionId) return;
      const previousStatus = state.externalJob?.id === job.id ? state.externalJob.status : null;
      state.externalJob = job;
      state.activityExternalJobs.set(job.id, job);
      renderExternalJob(job);
      renderActivityCenter();
      scheduleExternalJobPoll(job);
      if (["pending", "running"].includes(previousStatus) && !["pending", "running"].includes(job.status)) {
        refreshAfterExternalJob(collectionId);
      }
    } catch (error) {
      if (state.selected?.id !== collectionId) return;
      if (error.status === 404) {
        delete state.externalJobRefs[String(collectionId)];
        writeStorage(EXTERNAL_JOB_KEY, state.externalJobRefs);
        state.externalJob = null;
        ui.externalJobStatus.hidden = true;
        renderDataQualitySummary();
        return;
      }
      if (error.code === "application_busy") {
        if (!state.externalJob) {
          ui.externalJobStatus.hidden = false;
          ui.externalJobStatus.replaceChildren(el("p", "external-job-retry", "正在等待編目服務完成目前工作…"));
        }
        stopExternalJobPolling();
        state.externalJobTimer = window.setTimeout(() => loadExternalJob(jobId, collectionId), 500);
        return;
      }
      ui.externalJobStatus.hidden = false;
      ui.externalJobStatus.replaceChildren(el("p", "external-job-error", `無法更新外部搜尋狀態：${error.message}`));
    }
  }

  function renderExternalJob(job) {
    ui.externalJobStatus.hidden = false;
    ui.externalJobStatus.replaceChildren();
    const header = el("header", "external-job-header");
    const heading = el("div", "");
    heading.append(el("small", "", `EXTERNAL JOB #${job.id}`), el("strong", "", "外部資料搜尋"));
    header.append(heading, el("span", `job-status status-${job.status}`, EXTERNAL_JOB_STATUS_LABELS[job.status] || job.status));
    const facts = el("p", "external-job-facts", `${job.fields.map((field) => METADATA_LABELS[field] || field).join("、")} · 已嘗試 ${job.attempts} 次 · 更新於 ${formatMetadataTime(job.updated_at)}`);
    ui.externalJobStatus.append(header, facts);

    if (job.result) {
      const result = el("dl", "external-job-result");
      [
        ["收到候選", job.result.candidates_received],
        ["自動套用", job.result.auto_applied],
        ["建議候選", job.result.suggestions],
        ["僅供追查", job.result.search_only],
        ["新增標籤", job.result.tags_applied],
      ].forEach(([label, value]) => result.append(el("dt", "", label), el("dd", "", value ?? 0)));
      ui.externalJobStatus.append(result);
      if (Array.isArray(job.result.issues) && job.result.issues.length) {
        const issues = el("ul", "external-job-issues");
        job.result.issues.forEach((issue) => issues.append(el("li", "", `${issue.field ? `${METADATA_LABELS[issue.field] || issue.field}：` : ""}${issue.message}`)));
        ui.externalJobStatus.append(issues);
      }
    }
    if (job.error_message) ui.externalJobStatus.append(el("p", "external-job-error", `${job.error_kind || "搜尋錯誤"}：${job.error_message}`));
    if (job.next_retry_at) ui.externalJobStatus.append(el("p", "external-job-retry", `預計 ${formatMetadataTime(job.next_retry_at)} 後依錯誤種類重試。`));

    const refresh = el("button", "text-button", ["pending", "running"].includes(job.status) ? "立即更新狀態" : "重新讀取結果");
    refresh.type = "button";
    refresh.addEventListener("click", () => loadExternalJob(job.id, job.collection_id));
    ui.externalJobStatus.append(refresh);
    renderDataQualitySummary();
  }

  function scheduleExternalJobPoll(job) {
    stopExternalJobPolling();
    if (!["pending", "running"].includes(job.status)) return;
    let delay = 1400;
    if (job.next_retry_at) {
      const remaining = new Date(job.next_retry_at).getTime() - Date.now();
      if (Number.isFinite(remaining) && remaining > delay) delay = Math.min(60000, remaining);
    }
    state.externalJobTimer = window.setTimeout(() => loadExternalJob(job.id, job.collection_id), delay);
  }

  function stopExternalJobPolling() {
    if (state.externalJobTimer != null) window.clearTimeout(state.externalJobTimer);
    state.externalJobTimer = null;
  }

  async function refreshAfterExternalJob(collectionId) {
    try {
      const collection = await api(`/api/collections/${collectionId}`);
      const history = await api(`/api/collections/${collectionId}/metadata`);
      if (state.selected?.id !== collectionId) return;
      state.metadataHistory = history;
      replaceSelected(collection);
      renderMetadataHistory(history);
      toast("外部搜尋已完成，metadata 證據已更新");
    } catch (error) {
      toast(`外部搜尋已結束，但重新載入證據失敗：${error.message}`, true);
    }
  }

  async function rebuildThumbnail() {
    if (!state.selected) return;
    const button = byId("rebuild-thumbnail-button");
    button.disabled = true;
    try {
      await api(`/api/collections/${state.selected.id}/thumbnail/rebuild`, { method: "POST" });
      state.activityThumbnailFailures.delete(state.selected.id);
      restartThumbnailCollection(state.selected.id);
      renderActivityCenter();
      toast("縮圖已排入重建");
    } catch (error) {
      toast(error.message, true);
    } finally {
      button.disabled = false;
    }
  }

  function replaceSelected(collection) {
    state.selected = collection;
    const index = state.items.findIndex((item) => item.id === collection.id);
    if (index >= 0) state.items[index] = collection;
    if (state.selectedIds.has(collection.id)) state.selectedRecords.set(collection.id, collection);
    renderDetail(collection);
    if (index >= collectionWindowStart && index < collectionWindowEnd) {
      const current = ui.results.querySelector(`[data-collection-id="${collection.id}"]`)?.closest(".collection-item");
      if (current) {
        unbindThumbnailsWithin(current);
        const replacement = document.createDocumentFragment();
        appendCollectionItems([collection], index, replacement);
        current.replaceWith(replacement);
      }
    }
  }

  function toggleCollectionSelection(collection, checked) {
    if (checked) {
      state.selectedIds.add(collection.id);
      state.selectedRecords.set(collection.id, collection);
    } else {
      state.selectedIds.delete(collection.id);
      state.selectedRecords.delete(collection.id);
    }
    if (state.selectedIds.size === 0) state.selectionContext = null;
    const checkbox = ui.results.querySelector(`[data-collection-id="${collection.id}"]`)?.closest(".collection-item")?.querySelector(".collection-checkbox");
    if (checkbox) updateSelectionCheckbox(checkbox, checked);
    updateSelectionUI();
  }

  function selectLoadedCollections() {
    state.items.forEach((collection) => {
      state.selectedIds.add(collection.id);
      state.selectedRecords.set(collection.id, collection);
    });
    syncResultCheckboxes();
    updateSelectionUI();
  }

  function invertLoadedSelection() {
    state.items.forEach((collection) => {
      if (state.selectedIds.has(collection.id)) {
        state.selectedIds.delete(collection.id);
        state.selectedRecords.delete(collection.id);
      } else {
        state.selectedIds.add(collection.id);
        state.selectedRecords.set(collection.id, collection);
      }
    });
    syncResultCheckboxes();
    updateSelectionUI();
  }

  function clearSelection() {
    state.selectedIds.clear();
    state.selectedRecords.clear();
    state.selectionContext = null;
    syncResultCheckboxes();
    updateSelectionUI();
  }

  function updateSelectionCheckbox(checkbox, checked = checkbox.checked) {
    checkbox.checked = checked;
    const title = checkbox.dataset.collectionTitle || "這本收藏";
    checkbox.setAttribute("aria-label", checked ? `從批次選取移除 ${title}` : `將 ${title} 加入批次選取`);
  }

  function syncResultCheckboxes() {
    ui.results.querySelectorAll(".collection-checkbox").forEach((checkbox) => {
      const id = Number(checkbox.closest(".collection-item")?.querySelector("[data-collection-id]")?.dataset.collectionId);
      updateSelectionCheckbox(checkbox, state.selectedIds.has(id));
    });
  }

  function updateSelectionUI() {
    if (!ui.selectionRail) return;
    const count = state.selectedIds.size;
    ui.selectionRail.hidden = count === 0;
    ui.selectionCount.textContent = String(count);
    ui.selectionWorkbenchLink.textContent = `前往工作台處理 ${formatNumber(count)} 筆`;
    updateLibrarySummary();
    updateWorkbenchBadge();
    if (state.route === "workbench") renderWorkbenchSelection();
  }

  function selectedCollections() {
    return Array.from(state.selectedIds, (id) => state.selectedRecords.get(id)).filter(Boolean);
  }

  function updateWorkbenchBadge() {
    const pending = state.candidates.filter((candidate) => candidate.decision === "pending").length;
    const count = state.selectedIds.size + pending;
    ui.workbenchCount.textContent = String(count);
    ui.workbenchCount.hidden = count === 0;
    ui.workbenchCount.title = `${state.selectedIds.size} 筆批次選取，${pending} 筆候選待裁決`;
  }

  async function loadReviewQueue({ preferredId = null } = {}) {
    if (state.reviewLoading) return;
    const requestNumber = ++state.reviewRequestNumber;
    state.reviewLoading = true;
    ui.reviewLoading.hidden = false;
    ui.reviewError.hidden = true;
    try {
      const page = await api(`/api/review-queue?kind=${encodeURIComponent(state.reviewKind)}&page=${state.reviewPage}&per_page=100`);
      if (requestNumber !== state.reviewRequestNumber) return;
      state.reviewItems = page.items || [];
      state.reviewTotal = page.pagination?.total || 0;
      state.reviewTotalPages = page.pagination?.total_pages || 0;
      if (!state.reviewItems.length && state.reviewPage > Math.max(1, state.reviewTotalPages)) {
        state.reviewPage = Math.max(1, state.reviewTotalPages);
        state.reviewLoading = false;
        return loadReviewQueue({ preferredId });
      }
      const preferredIndex = preferredId == null ? -1 : state.reviewItems.findIndex((item) => item.collection.id === preferredId && !state.reviewSkipped.has(item.collection.id));
      if (preferredIndex >= 0) state.reviewPosition = preferredIndex;
      else state.reviewPosition = Math.min(state.reviewPosition, Math.max(0, state.reviewItems.length - 1));
      const available = availableReviewIndices();
      if (available.length && !available.includes(state.reviewPosition)) state.reviewPosition = available.find((index) => index >= state.reviewPosition) ?? available[available.length - 1];
      state.reviewLoaded = true;
      updateReviewBadge();
      renderReviewQueue();
    } catch (error) {
      if (requestNumber !== state.reviewRequestNumber) return;
      ui.reviewError.hidden = false;
      ui.reviewErrorMessage.textContent = error.message;
      ui.reviewDesk.hidden = true;
      ui.reviewEmpty.hidden = true;
    } finally {
      if (requestNumber === state.reviewRequestNumber) {
        state.reviewLoading = false;
        ui.reviewLoading.hidden = true;
      }
    }
  }

  function updateReviewBadge() {
    ui.reviewCount.textContent = String(state.reviewTotal);
    ui.reviewCount.hidden = state.reviewTotal === 0;
  }

  function availableReviewIndices() {
    const indices = [];
    state.reviewItems.forEach((item, index) => {
      if (!state.reviewSkipped.has(item.collection.id)) indices.push(index);
    });
    return indices;
  }

  function currentReviewItem() {
    return state.reviewItems[state.reviewPosition] || null;
  }

  function reviewItemIssues(item) {
    if (!item) return { candidates: [], missing: [] };
    const candidates = (item.metadata?.fields || []).flatMap((field) =>
      (field.assertions || []).filter((assertion) => assertion.status === "candidate").map((assertion) => ({ type: "candidate", field: field.field, assertion, history: field })),
    );
    const collection = item.collection;
    const missing = [
      ["title", !collection.title], ["event", !collection.event], ["circle", !collection.circle],
      ["authors", !collection.authors?.length], ["parody", !collection.parody], ["classification", !collection.classification_top],
    ].filter(([, isMissing]) => isMissing).map(([field]) => ({
      type: "missing",
      field,
      history: (item.metadata?.fields || []).find((candidate) => candidate.field === field),
    }));
    return { candidates, missing };
  }

  function primaryReviewIssue(item) {
    const issues = reviewItemIssues(item);
    return issues.candidates[0] || issues.missing[0] || null;
  }

  function renderReviewQueue() {
    const available = availableReviewIndices();
    const skippedCount = state.reviewSkipped.size;
    ui.reviewKind.value = state.reviewKind;
    ui.reviewTotal.textContent = `${formatNumber(state.reviewTotal)} 本收藏需要人工處理`;
    ui.reviewPosition.textContent = skippedCount ? `本次已略過 ${formatNumber(skippedCount)} 本` : "完成裁決或補值後，Queue 會依最新狀態更新";
    ui.reviewEmpty.hidden = available.length > 0;
    ui.reviewDesk.hidden = available.length === 0;
    ui.resetReviewSkips.hidden = skippedCount === 0;
    if (!available.length) {
      ui.reviewEmptyMessage.textContent = state.reviewTotal > 0 ? "目前頁面的項目都在本次 session 略過清單中；重設略過後可再次處理。" : "缺少的主要欄位都已補齊，候選也都完成裁決。";
      return;
    }
    if (!available.includes(state.reviewPosition)) state.reviewPosition = available[0];
    const item = currentReviewItem();
    const collection = item.collection;
    const issues = reviewItemIssues(item);
    const primary = primaryReviewIssue(item);
    bindThumbnail(ui.reviewCover, collection.id);
    ui.reviewCover.alt = `${displayTitle(collection)}封面`;
    ui.reviewSource.textContent = collection.root?.source === "downloads" ? "新收藏" : "典藏庫";
    const globalPosition = (state.reviewPage - 1) * 100 + state.reviewPosition + 1;
    ui.reviewSequence.textContent = `REVIEW ${String(globalPosition).padStart(3, "0")} / ${String(state.reviewTotal).padStart(3, "0")}`;
    ui.reviewTitle.textContent = displayTitle(collection);
    ui.reviewFilename.textContent = collection.filename;
    ui.reviewContext.replaceChildren();
    [["場次", collection.event], ["社團", collection.circle], ["作者", collection.authors?.join("、")], ["原作", collection.parody || collection.parody_raw]].forEach(([label, value]) => {
      ui.reviewContext.append(el("dt", "", label), el("dd", value ? "" : "metadata-missing", value || "未設定"));
    });
    ui.reviewProblems.replaceChildren();
    issues.candidates.forEach((issue) => ui.reviewProblems.append(el("span", "review-problem candidate", `${METADATA_LABELS[issue.field]}候選`)));
    issues.missing.forEach((issue) => ui.reviewProblems.append(el("span", "review-problem missing", `缺${METADATA_LABELS[issue.field]}`)));
    renderReviewDecision(primary);
    renderReviewAllIssues(issues, primary);
    const hasCandidate = primary?.type === "candidate";
    ui.reviewAccept.disabled = !hasCandidate;
    ui.reviewReject.disabled = !hasCandidate;
    ui.reviewEdit.disabled = !primary;
    ui.reviewPrevious.disabled = state.reviewPage === 1 && state.reviewPosition === available[0];
    ui.reviewNext.disabled = state.reviewPage >= state.reviewTotalPages && state.reviewPosition === available[available.length - 1];
  }

  function renderReviewDecision(issue) {
    ui.reviewDecision.replaceChildren();
    if (!issue) return;
    const heading = el("header", "review-decision-heading");
    heading.append(el("span", "review-field-index", METADATA_LABELS[issue.field] || issue.field), el("h3", "", issue.type === "candidate" ? "是否採用這筆候選？" : `補齊${METADATA_LABELS[issue.field]}`));
    ui.reviewDecision.append(heading);
    if (issue.type === "missing") {
      const empty = el("div", "review-missing-callout");
      empty.append(el("strong", "", "目前值：未設定"), el("p", "", "這個主要欄位沒有有效 selection。使用手動編輯寫入後，原有 assertion 與來源歷史仍會保留。"));
      ui.reviewDecision.append(empty);
      return;
    }
    const selected = (issue.history.assertions || []).find((assertion) => assertion.selected);
    const comparison = el("div", "review-comparison");
    comparison.append(reviewEvidenceColumn("CURRENT / 目前採用", selected, issue.history.selection), reviewEvidenceColumn("CANDIDATE / 待裁決", issue.assertion, null, true));
    ui.reviewDecision.append(comparison);
  }

  function reviewEvidenceColumn(label, assertion, selection = null, candidate = false) {
    const column = el("section", `review-evidence-column${candidate ? " candidate" : ""}`);
    column.append(el("p", "review-column-label", label));
    if (!assertion) {
      column.append(el("strong", "metadata-missing", "未設定"), el("small", "", "沒有目前 selection"));
      return column;
    }
    const badges = el("div", "assertion-badges");
    badges.append(el("span", `evidence-badge source-${assertion.source}`, METADATA_SOURCE_LABELS[assertion.source] || assertion.source), el("span", `evidence-badge status-${assertion.status}`, ASSERTION_STATUS_LABELS[assertion.status] || assertion.status));
    column.append(badges, el("strong", "review-evidence-value", formatEvidenceValue(assertion.value)));
    column.append(el("p", "assertion-reference", assertion.source_reference || (assertion.parser_run_id ? `parser run #${assertion.parser_run_id}` : "沒有額外來源參照")));
    if (selection) column.append(el("small", "review-selection-kind", `selection：${SELECTION_KIND_LABELS[selection.selected_by] || selection.selected_by}`));
    if (assertion.reason) column.append(el("p", "assertion-reason", assertion.reason));
    if (assertion.confidence_total != null) column.append(confidenceEvidence(assertion.confidence_total, assertion.confidence));
    return column;
  }

  function renderReviewAllIssues(issues, primary) {
    ui.reviewAllIssues.replaceChildren();
    const all = [...issues.candidates, ...issues.missing].filter((issue) => issue !== primary);
    ui.reviewMoreEvidence.hidden = all.length === 0;
    if (!all.length) return;
    const list = el("ol", "review-issue-list");
    all.forEach((issue) => {
      const item = el("li", "review-issue-row");
      item.append(el("strong", "", issue.type === "candidate" ? `${METADATA_LABELS[issue.field]}候選` : `缺${METADATA_LABELS[issue.field]}`));
      if (issue.type === "candidate") {
        item.append(el("span", "", formatEvidenceValue(issue.assertion.value)), el("small", "", `${METADATA_SOURCE_LABELS[issue.assertion.source] || issue.assertion.source}${issue.assertion.confidence_total == null ? "" : ` · 信心 ${formatPercent(issue.assertion.confidence_total)}`}`));
        if (issue.assertion.reason) item.append(el("p", "", issue.assertion.reason));
      } else item.append(el("span", "metadata-missing", "目前未設定"));
      list.append(item);
    });
    ui.reviewAllIssues.append(list);
  }

  async function decideReviewCandidate(decision) {
    const item = currentReviewItem();
    const issue = primaryReviewIssue(item);
    if (!item || issue?.type !== "candidate") return;
    if (decision === "reject" && !window.confirm("拒絕後仍會保留證據，但這筆 assertion 不能再次選取。確定拒絕？")) return;
    [ui.reviewAccept, ui.reviewReject, ui.reviewEdit, ui.reviewSkip].forEach((button) => { button.disabled = true; });
    try {
      await api(`/api/collections/${item.collection.id}/metadata/${issue.field}/assertions/${issue.assertion.id}`, { method: "PATCH", body: { decision } });
      invalidateDerivedData({ library: true });
      await loadReviewQueue({ preferredId: item.collection.id });
      const remains = state.reviewItems.some((candidate) => candidate.collection.id === item.collection.id);
      toast(`${decision === "select" ? "已採用" : "已拒絕"} assertion #${issue.assertion.id}${remains ? "；這本收藏仍有其他待審問題" : "；已前進下一筆"}`);
    } catch (error) {
      toast(`${decision === "select" ? "無法採用" : "無法拒絕"} assertion #${issue.assertion.id}：${error.message}`, true);
      renderReviewQueue();
    }
  }

  function openReviewEditor() {
    const item = currentReviewItem();
    const issue = primaryReviewIssue(item);
    if (item && issue) openMetadataDialog(issue.field, item.collection);
  }

  function skipCurrentReviewItem() {
    const item = currentReviewItem();
    if (!item) return;
    state.reviewSkipped.add(item.collection.id);
    const next = availableReviewIndices().find((index) => index > state.reviewPosition);
    if (next != null) {
      state.reviewPosition = next;
      renderReviewQueue();
    } else if (state.reviewPage < state.reviewTotalPages) {
      state.reviewPage += 1;
      state.reviewPosition = 0;
      loadReviewQueue();
    } else renderReviewQueue();
  }

  function resetReviewSkips() {
    state.reviewSkipped.clear();
    state.reviewPage = 1;
    state.reviewPosition = 0;
    loadReviewQueue();
  }

  function openReviewDetail() {
    const item = currentReviewItem();
    if (!item) return;
    state.reviewReturnId = item.collection.id;
    navigateToCollection(item.collection);
  }

  function moveReviewPosition(direction) {
    const available = availableReviewIndices();
    const next = available.indexOf(state.reviewPosition) + direction;
    if (next >= 0 && next < available.length) {
      state.reviewPosition = available[next];
      renderReviewQueue();
      ui.reviewDesk.scrollIntoView({ block: "start", behavior: "smooth" });
    } else if (direction > 0 && state.reviewPage < state.reviewTotalPages) {
      state.reviewPage += 1;
      state.reviewPosition = 0;
      loadReviewQueue();
    } else if (direction < 0 && state.reviewPage > 1) {
      state.reviewPage -= 1;
      state.reviewPosition = 99;
      loadReviewQueue();
    }
  }

  function loadWorkbench() {
    renderWorkbenchSelection();
    if (!state.workbenchLoaded) loadTombstoneCandidates();
  }

  function renderWorkbenchSelection() {
    if (!ui.selectedCollectionList) return;
    const collections = selectedCollections();
    unbindThumbnailsWithin(ui.selectedCollectionList);
    ui.selectedCollectionList.replaceChildren();
    ui.selectionEmpty.hidden = collections.length !== 0;
    ui.batchTools.hidden = collections.length === 0;
    ui.workbenchSelectionSummary.textContent = collections.length
      ? state.selectionContext === "thumbnail_failures"
        ? `縮圖失敗工作清單包含 ${formatNumber(collections.length)} 筆收藏；可逐筆查看，或返回設定重試整批失敗項目。`
        : `本次操作清單包含 ${formatNumber(collections.length)} 筆已選收藏；目前查詢已載入 ${formatNumber(state.items.length)} 筆，共符合 ${formatNumber(state.total)} 筆。`
      : "目前沒有批次操作清單。";
    collections.forEach((collection, index) => {
      const item = el("li", "selected-collection-item");
      const cover = document.createElement("img");
      cover.className = "selected-cover";
      cover.alt = "";
      cover.width = 38;
      cover.height = 51;
      cover.loading = "lazy";
      bindThumbnail(cover, collection.id);
      const copy = document.createElement("div");
      copy.append(
        el("strong", "", displayTitle(collection)),
        el("small", "", `${collection.root?.source === "downloads" ? "新收藏" : "典藏庫"} · ${collection.filename}`),
      );
      const remove = el("button", "text-button", "移出清單");
      remove.type = "button";
      remove.addEventListener("click", () => toggleCollectionSelection(collection, false));
      const view = el("button", "text-button", "查看藏書");
      view.type = "button";
      view.addEventListener("click", () => navigateToCollection(collection));
      const actions = el("div", "selected-collection-actions");
      actions.append(view, remove);
      item.append(cover, copy, actions);
      ui.selectedCollectionList.append(item);
    });
  }

  function syncBatchMetadataField() {
    const isClassification = ui.batchMetadataForm.elements.field.value === "classification";
    const label = byId("batch-metadata-value-label");
    label.firstChild.textContent = isClassification ? "新的種類" : "新的原作";
    ui.batchMetadataForm.elements.value.placeholder = isClassification ? "例如：同人誌、商業誌或其他" : "";
  }

  function externalBatchRequest() {
    return {
      collection_ids: Array.from(state.selectedIds),
      fields: Array.from(document.querySelectorAll('[name="external-batch-field"]:checked'), (input) => input.value),
      strategy: document.querySelector('[name="external-batch-strategy"]:checked')?.value || "only_missing",
    };
  }

  async function preflightExternalBatch() {
    const request = externalBatchRequest();
    if (!request.collection_ids.length) {
      toast("請先選取要補齊 metadata 的收藏", true);
      return;
    }
    if (!request.fields.length) {
      toast("至少選擇一個 metadata 欄位", true);
      return;
    }
    const button = byId("prepare-external-batch");
    button.disabled = true;
    button.textContent = "正在檢查…";
    try {
      const preflight = await api("/api/external-search-batches/preflight", { method: "POST", body: request });
      state.externalBatchPreflight = { request, preflight };
      renderExternalBatchPreflight(preflight);
    } catch (error) {
      toast(`無法檢查批次搜尋範圍：${error.message}`, true);
    } finally {
      button.disabled = false;
      button.textContent = "檢查搜尋範圍";
    }
  }

  function renderExternalBatchPreflight(preflight) {
    ui.externalBatchPreflight.hidden = false;
    ui.externalBatchPreflight.replaceChildren();
    const summary = el("dl", "enrichment-summary");
    [
      ["已選", preflight.total],
      ["建立工作", preflight.will_enqueue],
      ["沿用工作", preflight.reused],
      ["略過", preflight.skipped],
      ["無需搜尋", preflight.unchanged],
    ].forEach(([label, value]) => summary.append(el("dt", "", label), el("dd", "", formatNumber(value))));
    const fieldNeeds = preflight.field_needs
      .filter((need) => need.count > 0)
      .map((need) => `${METADATA_LABELS[need.field] || need.field} ${formatNumber(need.count)}`)
      .join(" · ");
    ui.externalBatchPreflight.append(
      el("strong", "", "預檢完成"),
      summary,
      el("p", "", fieldNeeds || "沒有欄位需要搜尋。"),
    );
    if (preflight.insufficient_identifiers) {
      ui.externalBatchPreflight.append(el("p", "enrichment-warning", `${formatNumber(preflight.insufficient_identifiers)} 筆缺少 provider 可用的識別碼或辨識書名，將略過。`));
    }
    ui.externalBatchActions.hidden = preflight.will_enqueue + preflight.reused === 0;
  }

  function clearExternalBatchPreflight() {
    state.externalBatchPreflight = null;
    ui.externalBatchPreflight.hidden = true;
    ui.externalBatchPreflight.replaceChildren();
    ui.externalBatchActions.hidden = true;
  }

  async function startExternalBatch() {
    const prepared = state.externalBatchPreflight;
    if (!prepared) return;
    const button = byId("start-external-batch");
    button.disabled = true;
    button.textContent = "正在建立工作…";
    try {
      const batch = await api("/api/external-search-batches", { method: "POST", body: prepared.request });
      state.externalBatch = batch;
      batch.items.forEach((item) => {
        if (!item.job_id) return;
        state.externalJobRefs[String(item.collection_id)] = item.job_id;
      });
      writeStorage(EXTERNAL_JOB_KEY, state.externalJobRefs);
      clearExternalBatchPreflight();
      renderExternalBatch(batch);
      scheduleExternalBatchPoll(batch);
      refreshActivityCenter(true);
      toast(`已建立批次外部搜尋 #${batch.id}`);
    } catch (error) {
      toast(`無法建立批次外部搜尋：${error.message}`, true);
    } finally {
      button.disabled = false;
      button.textContent = "開始背景搜尋";
    }
  }

  function renderExternalBatch(batch) {
    ui.externalBatchResult.hidden = false;
    ui.externalBatchResult.replaceChildren();
    const heading = el("div", "enrichment-result-heading");
    heading.append(
      el("strong", "", `BATCH #${batch.id} · 外部資料補齊`),
      el("span", "job-status", batchStatusLabel(batch.summary)),
    );
    const summary = el("dl", "enrichment-summary");
    [
      ["等待", batch.summary.pending], ["處理中", batch.summary.running],
      ["完成", batch.summary.succeeded], ["部分完成", batch.summary.partial],
      ["失敗", batch.summary.failed], ["略過", batch.summary.skipped],
      ["無變更", batch.summary.unchanged], ["沿用", batch.summary.reused],
    ].forEach(([label, value]) => summary.append(el("dt", "", label), el("dd", "", formatNumber(value))));
    const links = el("div", "enrichment-result-links");
    const activity = el("button", "text-button", "在 Activity 查看");
    activity.type = "button";
    activity.addEventListener("click", () => setActivityPanelOpen(true));
    const review = el("a", "text-link", "前往品質審核 →");
    review.href = "#review";
    links.append(activity, review);
    if (batch.summary.partial) {
      const retry = el("button", "secondary-button", `重試 ${formatNumber(batch.summary.partial)} 筆部分完成`);
      retry.type = "button";
      retry.addEventListener("click", () => retryExternalBatch(batch.id, retry));
      links.prepend(retry);
    }
    ui.externalBatchResult.append(heading, summary, links);
    const failures = batch.items.filter((item) => ["partial", "failed"].includes(item.status));
    if (batch.summary.failed) {
      ui.externalBatchResult.append(el("p", "enrichment-warning", `${formatNumber(batch.summary.failed)} 筆為 typed terminal failure，保留在清單供定位；批次不會強制繞過 retry policy。暫時性錯誤會維持等待狀態並依 backoff 自動重試。`));
    }
    if (failures.length) {
      const list = el("ol", "enrichment-failure-list");
      failures.slice(0, 20).forEach((item) => {
        const row = el("li", "");
        const open = el("button", "text-button", `收藏 #${item.collection_id}`);
        open.type = "button";
        open.addEventListener("click", () => openActivityCollection(item.collection_id));
        row.append(open, el("span", "", item.error_message || EXTERNAL_JOB_STATUS_LABELS[item.status] || item.status));
        list.append(row);
      });
      ui.externalBatchResult.append(list);
    }
    renderActivityCenter();
  }

  async function retryExternalBatch(batchId, button) {
    button.disabled = true;
    button.textContent = "正在依既有規則重試…";
    try {
      const batch = await api(`/api/external-search-batches/${batchId}/retry`, { method: "POST" });
      state.externalBatch = batch;
      batch.items.forEach((item) => {
        if (item.job_id) state.externalJobRefs[String(item.collection_id)] = item.job_id;
      });
      writeStorage(EXTERNAL_JOB_KEY, state.externalJobRefs);
      renderExternalBatch(batch);
      scheduleExternalBatchPoll(batch);
      toast(`已建立重試批次 #${batch.id}`);
    } catch (error) {
      toast(`無法重試批次：${error.message}`, true);
      renderExternalBatch(state.externalBatch);
    }
  }

  function batchStatusLabel(summary) {
    if (summary.running) return "處理中";
    if (summary.pending) return "等待背景處理";
    if (summary.failed || summary.partial) return "需要檢查";
    return "已完成";
  }

  function scheduleExternalBatchPoll(batch) {
    if (state.externalBatchTimer != null) window.clearTimeout(state.externalBatchTimer);
    if (!batch.summary.pending && !batch.summary.running) return;
    state.externalBatchTimer = window.setTimeout(async () => {
      try {
        const refreshed = await api(`/api/external-search-batches/${batch.id}`);
        state.externalBatch = refreshed;
        renderExternalBatch(refreshed);
        scheduleExternalBatchPoll(refreshed);
      } catch (error) {
        toast(`無法更新批次搜尋 #${batch.id}：${error.message}`, true);
      }
    }, 4000);
  }

  async function batchAddTag(event) {
    event.preventDefault();
    const name = String(new FormData(ui.batchTagForm).get("tag") || "").trim();
    if (!name) {
      toast("請輸入要加入的標籤", true);
      return;
    }
    const collections = selectedCollections();
    const completed = await runBatchOperation({
      title: `批次加入標籤「${name}」`,
      endpoint: "/api/batch/tags",
      method: "POST",
      payload: { name },
      collections,
    });
    if (completed) {
      ui.batchTagForm.reset();
      closeTagSuggestionsFor(ui.batchTagForm.elements.tag);
    }
  }

  async function batchSetMetadata(event) {
    event.preventDefault();
    const form = new FormData(ui.batchMetadataForm);
    const field = String(form.get("field"));
    const value = String(form.get("value") || "").trim();
    if (!value) {
      toast(`請輸入新的${METADATA_LABELS[field]}`, true);
      return;
    }
    const completed = await runBatchOperation({
      title: `批次寫入${METADATA_LABELS[field]}「${value}」`,
      endpoint: `/api/batch/metadata/${field}`,
      method: "PUT",
      payload: { value },
      collections: selectedCollections(),
    });
    if (completed) ui.batchMetadataForm.elements.value.value = "";
  }

  async function retryFailedBatch() {
    if (!state.batchRetry) return;
    await runBatchOperation(state.batchRetry);
  }

  async function runBatchOperation(operation) {
    const collections = operation.collections || [];
    if (!collections.length) return false;
    const submitters = document.querySelectorAll("#batch-tools button, #retry-batch-failures");
    submitters.forEach((button) => { button.disabled = true; });
    updateBatchProgress(operation.title, 0, collections.length, "後端正在處理批次要求");
    state.batchRunning = { title: operation.title, completed: 0, total: collections.length };
    state.lastBatchActivity = null;
    renderActivityCenter();
    const outcomes = { succeeded: [], unchanged: [], failed: [] };
    try {
      for (let offset = 0; offset < collections.length; offset += BATCH_REQUEST_SIZE) {
        const chunk = collections.slice(offset, offset + BATCH_REQUEST_SIZE);
        try {
          const report = await api(operation.endpoint, {
            method: operation.method,
            body: {
              ...operation.payload,
              collection_ids: chunk.map((collection) => collection.id),
            },
          });
          const chunkOutcomes = batchOutcomes(report, chunk);
          outcomes.succeeded.push(...chunkOutcomes.succeeded);
          outcomes.unchanged.push(...chunkOutcomes.unchanged);
          outcomes.failed.push(...chunkOutcomes.failed);
          chunkOutcomes.succeeded.concat(chunkOutcomes.unchanged).forEach(({ result }) => mergeBatchCollection(result));
        } catch (error) {
          outcomes.failed.push(...chunk.map((collection) => ({ collection, error })));
        }
        state.batchRunning.completed = Math.min(collections.length, offset + chunk.length);
        updateBatchProgress(operation.title, state.batchRunning.completed, collections.length, "後端正在處理批次要求");
        renderActivityCenter();
      }
      updateBatchProgress(operation.title, collections.length, collections.length, outcomes.failed.length ? "批次要求部分完成" : "批次要求已完成");
      const failedCollections = outcomes.failed.map(({ collection }) => collection);
      state.batchRetry = failedCollections.length ? { ...operation, collections: failedCollections } : null;
      renderWorkbenchSelection();
      renderClientBatchResult(operation.title, outcomes);
      await synchronizeLibraryAfterBatch();
      return true;
    } finally {
      state.batchRunning = null;
      renderActivityCenter();
      submitters.forEach((button) => { button.disabled = false; });
    }
  }

  function batchOutcomes(report, collections) {
    const originals = new Map(collections.map((collection) => [collection.id, collection]));
    const outcomes = { succeeded: [], unchanged: [], failed: [] };
    report.items.forEach((item) => {
      const collection = item.collection || originals.get(item.collection_id) || { id: item.collection_id, title: `收藏 #${item.collection_id}` };
      if (item.status === "succeeded") outcomes.succeeded.push({ collection, result: item.collection });
      else if (item.status === "unchanged") outcomes.unchanged.push({ collection, result: item.collection });
      else outcomes.failed.push({ collection, error: item.error || { message: "批次操作失敗" } });
    });
    return outcomes;
  }

  function mergeBatchCollection(collection) {
    if (!collection) return;
    if (state.selectedIds.has(collection.id)) state.selectedRecords.set(collection.id, collection);
    const index = state.items.findIndex((item) => item.id === collection.id);
    if (index >= 0) state.items[index] = collection;
    if (state.selected?.id === collection.id) state.selected = collection;
  }

  async function synchronizeLibraryAfterBatch() {
    invalidateDerivedData({ library: true });
    state.libraryFocusId = null;
    if (state.route === "library") {
      await loadCollections({ preserveSelection: true });
    }
  }

  function updateBatchProgress(title, completed, total, label) {
    ui.batchProgress.hidden = false;
    ui.batchProgressLabel.textContent = `${title} · ${label}`;
    ui.batchProgressCount.textContent = `${formatNumber(completed)} / ${formatNumber(total)}`;
    ui.batchProgressBar.max = Math.max(1, total);
    ui.batchProgressBar.value = completed;
  }

  function renderClientBatchResult(title, outcomes) {
    const summary = `更新 ${outcomes.succeeded.length} 筆，未變更 ${outcomes.unchanged.length} 筆，失敗 ${outcomes.failed.length} 筆`;
    ui.batchResult.hidden = false;
    ui.retryBatchFailures.hidden = outcomes.failed.length === 0;
    ui.batchResultSummary.replaceChildren();
    ui.batchResultSummary.append(
      el("strong", "", title),
      el("span", "", summary),
    );
    ui.batchResultItems.replaceChildren();
    outcomes.succeeded.forEach(({ collection }) => ui.batchResultItems.append(batchResultItem(collection, "succeeded", "完成")));
    outcomes.unchanged.forEach(({ collection }) => ui.batchResultItems.append(batchResultItem(collection, "unchanged", "已具有相同值")));
    outcomes.failed.forEach(({ collection, error }) => ui.batchResultItems.append(batchResultItem(collection, "failed", error.message)));
    recordBatchActivity(title, summary, outcomes.failed.length);
    toast(outcomes.failed.length ? `${title}部分完成` : `${title}完成`, outcomes.failed.length > 0);
  }

  function recordBatchActivity(title, summary, failed) {
    state.lastBatchActivity = { title, summary, failed, updatedAt: new Date().toISOString() };
    renderActivityCenter();
  }

  function batchResultItem(collection, status, message) {
    const item = el("li", `result-${status}`);
    item.append(el("strong", "", displayTitle(collection)), el("span", "", message));
    return item;
  }

  async function prepareMove() {
    const collections = selectedCollections();
    if (!collections.length) return;
    try {
      const data = await api("/api/library-roots");
      const roots = data.roots.filter((root) => root.active && root.source === "archive");
      if (!roots.length) {
        toast("尚未設定啟用中的典藏庫。請先到設定登記典藏庫。", true);
        return;
      }
      ui.archiveRootSelect.replaceChildren();
      roots.forEach((root) => {
        const option = document.createElement("option");
        option.value = String(root.id);
        option.textContent = `${root.label} — ${root.path}`;
        ui.archiveRootSelect.append(option);
      });
      byId("move-summary").textContent = `${selectionImpactSummary("搬移", collections.length)}只有新收藏來源可以搬移；其他項目會逐筆回報失敗。`;
      renderConfirmItems(byId("move-item-list"), collections);
      ui.moveDialog.showModal();
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function executeMove(event) {
    event.preventDefault();
    const collections = selectedCollections();
    if (!collections.length) return;
    const submit = ui.moveForm.querySelector('[type="submit"]');
    submit.disabled = true;
    submit.textContent = "正在搬移…";
    try {
      const report = await api("/api/file-actions/move", {
        method: "POST",
        body: { collection_ids: collections.map((collection) => collection.id), archive_root_id: Number(ui.archiveRootSelect.value) },
      });
      ui.moveDialog.close();
      applyFileReport("搬移", report, collections);
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
      submit.textContent = "搬移已選收藏";
    }
  }

  function prepareDelete() {
    const collections = selectedCollections();
    if (!collections.length) return;
    ui.deleteForm.reset();
    ui.deleteForm.elements.mode.value = "soft";
    renderConfirmItems(byId("delete-item-list"), collections);
    syncDeleteMode();
    ui.deleteDialog.showModal();
  }

  function syncDeleteMode() {
    const permanent = ui.deleteForm.elements.mode.value === "permanent";
    const phrase = `永久刪除 ${state.selectedIds.size} 筆`;
    byId("delete-summary").textContent = selectionImpactSummary(permanent ? "永久刪除" : "移到資源回收桶");
    ui.permanentConfirmPhrase.textContent = phrase;
    ui.permanentConfirmGroup.hidden = !permanent;
    byId("permanent-confirm-note").hidden = !permanent;
    const submit = byId("confirm-delete");
    submit.textContent = permanent ? `永久刪除 ${state.selectedIds.size} 筆` : "移到資源回收桶";
    submit.disabled = permanent && ui.deleteForm.elements.confirmation.value !== phrase;
  }

  function selectionImpactSummary(action, selectedCount = state.selectedIds.size) {
    const queryTotal = Math.max(Number(state.total) || 0, selectedCount);
    const unaffectedCount = Math.max(0, queryTotal - selectedCount);
    const impact = action === "移到資源回收桶"
      ? `將把已選的 ${formatNumber(selectedCount)} 筆移到資源回收桶`
      : `將${action}已選的 ${formatNumber(selectedCount)} 筆`;
    return `${impact}。此查詢共 ${formatNumber(queryTotal)} 筆，其餘 ${formatNumber(unaffectedCount)} 筆不受影響。`;
  }

  async function executeDelete(event) {
    event.preventDefault();
    const collections = selectedCollections();
    if (!collections.length) return;
    const mode = ui.deleteForm.elements.mode.value;
    const phrase = `永久刪除 ${collections.length} 筆`;
    if (mode === "permanent" && ui.deleteForm.elements.confirmation.value !== phrase) {
      toast(`請輸入「${phrase}」確認永久刪除`, true);
      return;
    }
    const submit = byId("confirm-delete");
    submit.disabled = true;
    submit.textContent = mode === "permanent" ? "正在永久刪除…" : "正在移到資源回收桶…";
    try {
      const report = await api("/api/file-actions/delete", {
        method: "POST",
        body: { collection_ids: collections.map((collection) => collection.id), mode },
      });
      ui.deleteDialog.close();
      applyFileReport(mode === "permanent" ? "永久刪除" : "軟刪除", report, collections);
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
      syncDeleteMode();
    }
  }

  function renderConfirmItems(container, collections) {
    container.replaceChildren();
    collections.forEach((collection) => {
      const item = el("li", "");
      item.append(el("strong", "", displayTitle(collection)), el("small", "", collection.path));
      container.append(item);
    });
  }

  function applyFileReport(action, report, collections) {
    const byIdMap = new Map(collections.map((collection) => [collection.id, collection]));
    ui.batchResult.hidden = false;
    ui.batchResultSummary.replaceChildren(
      el("strong", "", `${action}結果`),
      el("span", "", `成功 ${report.succeeded}、失敗 ${report.failed}、待復原 ${report.pending_recovery}`),
    );
    ui.batchResultItems.replaceChildren();
    report.items.forEach((entry) => {
      const collection = byIdMap.get(entry.collection_id) || { id: entry.collection_id, title: `收藏 #${entry.collection_id}` };
      const message = entry.status === "succeeded" ? "完成" : entry.error || (entry.status === "pending_recovery" ? "狀態待人工復原" : "操作失敗");
      ui.batchResultItems.append(batchResultItem(collection, entry.status, message));
      if (entry.status === "succeeded") {
        state.selectedIds.delete(entry.collection_id);
        state.selectedRecords.delete(entry.collection_id);
      }
    });
    state.items = [];
    state.selected = null;
    invalidateDerivedData({ library: true });
    updateSelectionUI();
    recordBatchActivity(`${action}結果`, `成功 ${report.succeeded}、失敗 ${report.failed}、待復原 ${report.pending_recovery}`, report.failed + report.pending_recovery);
    toast(report.failed || report.pending_recovery ? `${action}部分完成，請查看逐筆結果` : `${action}完成`, Boolean(report.failed || report.pending_recovery));
  }

  async function loadTombstoneCandidates() {
    ui.candidateLoading.hidden = false;
    ui.candidateEmpty.hidden = true;
    ui.candidateGroups.hidden = true;
    try {
      const data = await api("/api/tombstone-candidates");
      state.candidates = data.items;
      state.workbenchLoaded = true;
      renderTombstoneCandidates();
      updateWorkbenchBadge();
    } catch (error) {
      toast(error.message, true);
    } finally {
      ui.candidateLoading.hidden = true;
    }
  }

  function renderTombstoneCandidates() {
    unbindThumbnailsWithin(ui.candidateGroups);
    ui.candidateGroups.replaceChildren();
    ui.candidateGroups.hidden = state.candidates.length === 0;
    ui.candidateEmpty.hidden = state.candidates.length !== 0;
    const groups = new Map();
    state.candidates.forEach((candidate) => {
      const group = groups.get(candidate.tombstone_collection_id) || [];
      group.push(candidate);
      groups.set(candidate.tombstone_collection_id, group);
    });
    groups.forEach((candidates, tombstoneId) => {
      const group = el("section", "candidate-group");
      const header = el("header", "candidate-group-header");
      const heading = document.createElement("div");
      heading.append(
        el("span", "identity-id", `TOMBSTONE #${tombstoneId}`),
        el("h3", "", filenameFromPath(candidates[0].tombstone_path)),
        el("code", "", candidates[0].tombstone_path),
      );
      const pending = candidates.filter((candidate) => candidate.decision === "pending").length;
      header.append(heading, el("span", `decision-badge ${pending ? "pending" : "decided"}`, pending ? `待裁決 ${pending} 筆` : "全部已裁決"));
      const list = el("ol", "candidate-list");
      candidates
        .slice()
        .sort((left, right) => decisionOrder(left.decision) - decisionOrder(right.decision))
        .forEach((candidate) => list.append(renderCandidate(candidate)));
      group.append(header, list);
      ui.candidateGroups.append(group);
    });
  }

  function renderCandidate(candidate) {
    const item = el("li", "candidate-item");
    const cover = document.createElement("img");
    cover.alt = "";
    cover.width = 72;
    cover.height = 96;
    cover.loading = "lazy";
    bindThumbnail(cover, candidate.candidate_collection_id);
    const copy = el("div", "candidate-copy");
    copy.append(
      el("span", "identity-id", `CANDIDATE #${candidate.candidate_collection_id}`),
      el("strong", "", filenameFromPath(candidate.candidate_path) || "候選位置已不存在"),
      el("code", "", candidate.candidate_path || "沒有目前路徑"),
      el("small", "", candidateReason(candidate.reason)),
    );
    const decision = el("div", "candidate-decision");
    decision.append(el("span", `decision-badge ${candidate.decision}`, decisionLabel(candidate.decision)));
    const actions = el("div", "candidate-actions");
    if (candidate.decision !== "confirmed") {
      const confirm = el("button", "primary-button", "標記為同一收藏");
      confirm.type = "button";
      confirm.addEventListener("click", () => decideCandidate(candidate, "confirmed"));
      actions.append(confirm);
    }
    if (candidate.decision !== "rejected") {
      const reject = el("button", "text-button danger-text", "排除這個候選");
      reject.type = "button";
      reject.addEventListener("click", () => decideCandidate(candidate, "rejected"));
      actions.append(reject);
    }
    if (candidate.decision === "confirmed") {
      const preflight = el("button", "primary-button accent-button", "執行合併預檢");
      preflight.type = "button";
      preflight.addEventListener("click", () => openConsolidationPreflight(candidate));
      actions.prepend(preflight);
    }
    decision.append(actions);
    item.append(cover, copy, decision);
    return item;
  }

  async function decideCandidate(candidate, decision) {
    const action = decision === "confirmed" ? "標記為同一收藏" : "排除候選";
    try {
      await api(`/api/tombstone-candidates/${candidate.tombstone_collection_id}/${candidate.candidate_collection_id}`, {
        method: "PATCH",
        body: { decision },
      });
      toast(`${action}完成；收藏身分尚未合併`);
      await loadTombstoneCandidates();
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function openConsolidationPreflight(candidate) {
    try {
      const preflight = await api(`/api/tombstone-candidates/${candidate.tombstone_collection_id}/${candidate.candidate_collection_id}/preflight`);
      state.preflight = preflight;
      state.preflightPair = candidate;
      renderConsolidationPreflight();
      ui.consolidationDialog.showModal();
    } catch (error) {
      toast(error.message, true);
    }
  }

  function renderConsolidationPreflight() {
    const preflight = state.preflight;
    const candidate = state.preflightPair;
    const phrase = `合併 ${preflight.tombstone_collection_id} <- ${preflight.candidate_collection_id}`;
    ui.consolidationConfirmPhrase.textContent = phrase;
    ui.consolidationForm.elements.confirmation.value = "";
    byId("identity-pair").replaceChildren(
      identitySide("存活身分", `#${preflight.tombstone_collection_id}`, candidate.tombstone_path),
      el("span", "identity-arrow", "←"),
      identitySide("接管位置", `#${preflight.candidate_collection_id}`, candidate.candidate_path || "位置已不存在"),
    );

    const blockers = preflight.blockers || [];
    ui.preflightBlockers.hidden = blockers.length === 0;
    const blockerList = ui.preflightBlockers.querySelector("ul");
    blockerList.replaceChildren();
    blockers.forEach((blocker) => blockerList.append(el("li", "", blocker.message)));

    const conflicts = preflight.conflicts || [];
    ui.conflictSection.hidden = conflicts.length === 0;
    ui.conflictList.replaceChildren();
    conflicts.forEach((conflict) => {
      const fieldset = el("fieldset", "conflict-choice");
      fieldset.dataset.field = conflict.field;
      const legend = el("legend", "", METADATA_LABELS[conflict.field] || conflict.field);
      fieldset.append(
        legend,
        conflictOption(conflict.field, "tombstone", "保留舊收藏值", conflict.tombstone),
        conflictOption(conflict.field, "candidate", "採用候選值", conflict.candidate),
      );
      ui.conflictList.append(fieldset);
    });

    byId("consolidation-confirm-group").hidden = preflight.already_consolidated;
    ui.confirmConsolidation.textContent = preflight.already_consolidated ? "已完成合併" : "合併收藏身分";
    syncConsolidationConfirmation();
  }

  function identitySide(label, id, path) {
    const side = el("div", "identity-side");
    side.append(el("small", "", label), el("strong", "", id), el("code", "", path));
    return side;
  }

  function conflictOption(field, choice, label, evidence) {
    const option = el("label", "conflict-option");
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = `resolution-${field}`;
    radio.value = choice;
    const copy = document.createElement("span");
    copy.append(
      el("strong", "", label),
      el("b", "", formatEvidenceValue(evidence.value)),
      el("small", "", `${evidence.source} assertion #${evidence.assertion_id}`),
    );
    option.append(radio, copy);
    return option;
  }

  function syncConsolidationConfirmation() {
    if (!state.preflight) return;
    const preflight = state.preflight;
    const phrase = `合併 ${preflight.tombstone_collection_id} <- ${preflight.candidate_collection_id}`;
    const choices = ui.conflictList.querySelectorAll('input[type="radio"]:checked').length;
    const allResolved = choices === (preflight.conflicts || []).length;
    const confirmed = ui.consolidationForm.elements.confirmation.value === phrase;
    ui.confirmConsolidation.disabled = preflight.already_consolidated || preflight.blockers.length > 0 || !allResolved || !confirmed;
  }

  async function executeConsolidation(event) {
    event.preventDefault();
    if (!state.preflight || ui.confirmConsolidation.disabled) return;
    const preflight = state.preflight;
    const resolutions = (preflight.conflicts || []).map((conflict) => ({
      field: conflict.field,
      choice: ui.consolidationForm.querySelector(`input[name="resolution-${conflict.field}"]:checked`).value,
    }));
    ui.confirmConsolidation.disabled = true;
    ui.confirmConsolidation.textContent = "正在合併身分…";
    try {
      const result = await api(`/api/tombstone-candidates/${preflight.tombstone_collection_id}/${preflight.candidate_collection_id}/consolidate`, {
        method: "POST",
        body: { resolutions },
      });
      ui.consolidationDialog.close();
      ui.identityResult.hidden = false;
      ui.identityResult.replaceChildren(
        el("strong", "", `身分合併完成 · survivor #${result.survivor_collection_id}`),
        el("span", "", `candidate #${result.merged_collection_id} 已成為稽核紀錄；實體 ZIP 沒有搬移。`),
      );
      state.items = [];
      invalidateDerivedData({ library: true });
      state.workbenchLoaded = false;
      toast("收藏身分已完成合併");
      await loadTombstoneCandidates();
    } catch (error) {
      toast(error.message, true);
      syncConsolidationConfirmation();
    } finally {
      ui.confirmConsolidation.textContent = "合併收藏身分";
    }
  }

  function decisionOrder(decision) {
    return { pending: 0, confirmed: 1, rejected: 2 }[decision] ?? 3;
  }

  function decisionLabel(decision) {
    return { pending: "待裁決", confirmed: "已確認同一收藏", rejected: "已排除" }[decision] || decision;
  }

  function candidateReason(reason) {
    return reason === "same_filename" ? "掃描發現相同檔名" : reason;
  }

  function filenameFromPath(path) {
    if (!path) return "";
    return path.split(/[\\/]/).filter(Boolean).at(-1) || path;
  }

  function formatEvidenceValue(value) {
    if (Array.isArray(value)) return value.join("、");
    if (value && typeof value === "object") {
      if (value.top_level) return [value.top_level, value.subcategory].filter(Boolean).join("／");
      if (value.canonical || value.raw) return value.canonical || value.raw;
      if (value.values) return value.values.join("、");
      return JSON.stringify(value);
    }
    if (typeof value === "boolean") return value ? "是" : "否";
    return String(value ?? "未設定");
  }

  async function loadStats() {
    if (state.statsLoaded) return;
    ui.statLedger.replaceChildren(el("p", "loading-state", "正在計算收藏統計…"));
    try {
      const stats = state.statsData || await api("/api/stats");
      state.statsData = stats;
      renderStats(stats);
      state.statsLoaded = true;
    } catch (error) {
      ui.statLedger.replaceChildren(el("p", "", "無法載入統計。"));
      toast(error.message, true);
    }
  }

  function renderStats(stats) {
    ui.statLedger.replaceChildren();
    const taggedRate = stats.total ? Math.round((stats.tagged / stats.total) * 100) : 0;
    ui.statLedger.append(
      statNumber(stats.total, "目前有效收藏"),
      statNumber(stats.tagged, `已有標籤 · ${taggedRate}%`),
      statNumber(stats.missing_metadata, "缺少 metadata", "attention"),
    );
    ui.statColumns.replaceChildren();
    [
      ["種類", stats.categories, "classification"],
      ["常見原作", stats.top_parody, "parody"],
      ["主要場次", stats.top_event, "event"],
      ["常見作者", stats.top_author, "author"],
      ["常見社團", stats.top_circle, "circle"],
      ["標籤涵蓋", stats.top_tags, "tag"],
    ].forEach(([title, rows, filter]) => ui.statColumns.append(statTable(title, rows, filter)));
  }

  function statNumber(value, label, className = "") {
    const box = el("div", `stat-number${className ? ` ${className}` : ""}`);
    box.append(el("strong", "", formatNumber(value)), el("span", "", label));
    return box;
  }

  function statTable(title, rows, filterName) {
    const section = el("section", "stat-table-section");
    section.append(el("h2", "", title));
    const table = el("table", "stat-table");
    const head = document.createElement("thead");
    const headRow = document.createElement("tr");
    headRow.append(el("th", "", "名稱"), el("th", "", "筆數"));
    head.append(headRow);
    const body = document.createElement("tbody");
    const max = rows?.[0]?.count || 1;
    (rows || []).slice(0, 12).forEach((row) => {
      const tr = document.createElement("tr");
      const name = document.createElement("td");
      const bar = el("span", "stat-bar");
      const widthStep = Math.max(1, Math.min(10, Math.ceil((row.count / max) * 10)));
      bar.classList.add(`w-${widthStep}`);
      bar.setAttribute("aria-hidden", "true");
      const filter = el("button", "stat-filter", row.name);
      filter.type = "button";
      filter.title = `在藏書中篩選「${row.name}」`;
      filter.addEventListener("click", () => applyFilter(filterName, row.name));
      name.append(bar, filter);
      tr.append(name, el("td", "", formatNumber(row.count)));
      body.append(tr);
    });
    if (!rows?.length) {
      const tr = document.createElement("tr");
      const td = el("td", "", "尚無資料");
      td.colSpan = 2;
      tr.append(td);
      body.append(tr);
    }
    table.append(head, body);
    section.append(table);
    return section;
  }

  async function loadSettingsPage() {
    stopThumbnailCachePolling();
    try {
      const [settings, roots, cacheJobs] = await Promise.all([
        api("/api/settings"),
        api("/api/library-roots"),
        api("/api/thumbnail-cache-jobs/current"),
      ]);
      ui.settingsForm.elements.viewer_path.value = settings.viewer_path;
      ui.settingsForm.elements.thumb_size.value = settings.thumb_size;
      ui.settingsForm.elements.thumb_quality.value = settings.thumb_quality;
      state.settingsSnapshot = settings;
      syncSettingsOverride("viewer_path", ui.viewerPathOverride, settings.overrides.viewer_path, settings.viewer_path, settings.saved_viewer_path);
      syncSettingsOverride("thumb_size", ui.thumbSizeOverride, settings.overrides.thumb_size, settings.thumb_size, settings.saved_thumb_size);
      syncSettingsOverride("thumb_quality", ui.thumbQualityOverride, settings.overrides.thumb_quality, settings.thumb_quality, settings.saved_thumb_quality);
      ui.environmentOverrides.textContent = settings.environment_overrides.length
        ? `有 ${formatNumber(settings.environment_overrides.length)} 個欄位由環境變數控制，已在各欄位旁標示目前有效值與已儲存值。`
        : "目前沒有環境變數覆寫；這裡儲存的值會直接生效。";
      state.settingsRoots = roots.roots;
      ui.rootRescanNote.hidden = !state.rootsNeedScan;
      updateThumbnailCacheJob(cacheJobs.job, { announce: false });
      renderRoots(roots.roots);
      renderThumbnailCacheRoots();
      renderThumbnailCacheProgress();
      scheduleThumbnailCachePolling();
      focusRequestedRootSettings();
    } catch (error) {
      toast(error.message, true);
    }
  }

  function focusRequestedRootSettings() {
    const mode = state.settingsRootFocus;
    if (!mode) return;
    state.settingsRootFocus = null;
    window.requestAnimationFrame(() => {
      const target = mode === "new" ? ui.rootForm.elements.label : ui.rootsHeading;
      target.scrollIntoView({ block: "center" });
      target.focus({ preventScroll: true });
    });
  }

  function syncSettingsOverride(fieldName, note, environmentName, effectiveValue, savedValue) {
    const input = ui.settingsForm.elements[fieldName];
    input.disabled = Boolean(environmentName);
    note.hidden = !environmentName;
    if (!environmentName) {
      note.textContent = "";
      return;
    }
    note.textContent = `${environmentName} 控制中：目前有效值為「${effectiveValue || "系統預設"}」；UI 已儲存值為「${savedValue || "系統預設"}」。儲存本頁不會改變目前有效值。`;
  }

  function renderThumbnailCacheRoots() {
    ui.thumbnailCacheRoots.replaceChildren();
    const activeRoots = state.settingsRoots.filter((root) => root.active);
    if (!activeRoots.length) {
      ui.thumbnailCacheRoots.append(el("p", "field-note", "目前沒有已啟用的資料夾來源。"));
      ui.thumbnailCacheStart.disabled = true;
      return;
    }
    const running = state.thumbnailCacheJob?.status === "running";
    const selectedIds = state.thumbnailCacheJob
      ? new Set(state.thumbnailCacheJob.root_ids)
      : new Set(activeRoots.map((root) => root.id));
    activeRoots.forEach((root) => {
      const label = el("label", "thumbnail-cache-root");
      const checkbox = document.createElement("input");
      checkbox.type = "checkbox";
      checkbox.name = "root_id";
      checkbox.value = String(root.id);
      checkbox.checked = selectedIds.has(root.id);
      checkbox.disabled = running || state.thumbnailCacheRetrying;
      const copy = el("span", "", root.label);
      copy.append(el("small", "", root.path));
      label.append(checkbox, copy);
      ui.thumbnailCacheRoots.append(label);
    });
    ui.thumbnailCacheStart.disabled = running || state.thumbnailCacheRetrying;
    ui.thumbnailCacheStart.textContent = running ? "建立中…" : state.thumbnailCacheRetrying ? "重新排入中…" : "開始建立";
  }

  function renderThumbnailCacheProgress() {
    const job = state.thumbnailCacheJob;
    ui.thumbnailCacheProgress.hidden = !job;
    if (!job) return;
    const running = job.status === "running";
    const hasErrors = job.status === "completed_with_errors";
    const percent = formatProgressPercent(job.progress_percent);
    ui.thumbnailCacheProgress.className = `thumbnail-cache-progress${hasErrors ? " has-errors" : running ? "" : " is-completed"}`;
    ui.thumbnailCachePercent.textContent = percent;
    ui.thumbnailCacheProgressBar.value = job.progress_percent;
    ui.thumbnailCacheProgressBar.textContent = percent;
    ui.thumbnailCacheActions.hidden = !hasErrors;
    ui.thumbnailCacheViewFailures.textContent = `查看 ${formatNumber(job.failed)} 本失敗收藏`;
    ui.thumbnailCacheRetryFailures.disabled = state.thumbnailCacheRetrying;
    ui.thumbnailCacheRetryFailures.textContent = state.thumbnailCacheRetrying ? "重新排入中…" : "重試失敗項目";
    if (running) {
      ui.thumbnailCacheStatus.textContent = "正在建立快取縮圖";
      if (Number(job.progress_percent) === 0 && job.running > 0) {
        ui.thumbnailCacheDetail.textContent = `已有 ${formatNumber(job.running)} 張處理中、${formatNumber(job.pending)} 張等待，已經過 ${formatDuration(job.elapsed_seconds)} · ${formatThumbnailEta(job.estimated_seconds_remaining)}。本批開始後無法中止，可離開此頁繼續背景處理。`;
      } else {
        ui.thumbnailCacheDetail.textContent = `完成 ${formatNumber(job.ready)}、建立中 ${formatNumber(job.running)}、等待 ${formatNumber(job.pending)} · ${formatThumbnailEta(job.estimated_seconds_remaining)}。本批開始後無法中止，可離開此頁繼續背景處理。`;
      }
    } else if (hasErrors) {
      ui.thumbnailCacheStatus.textContent = "快取縮圖已部分完成";
      ui.thumbnailCacheDetail.textContent = `完成 ${formatNumber(job.ready)} 張，${formatNumber(job.failed)} 張失敗；可直接查看失敗收藏或整批重新排入。`;
    } else if (job.total === 0) {
      ui.thumbnailCacheStatus.textContent = "所選範圍沒有收藏";
      ui.thumbnailCacheDetail.textContent = "不需要建立快取縮圖。";
    } else {
      ui.thumbnailCacheStatus.textContent = "快取縮圖建立完成";
      ui.thumbnailCacheDetail.textContent = `所選範圍的 ${formatNumber(job.total)} 張縮圖皆已備妥。`;
    }
  }

  async function startThumbnailCacheJob(event) {
    event.preventDefault();
    const rootIds = [...ui.thumbnailCacheForm.querySelectorAll('input[name="root_id"]:checked')]
      .map((input) => Number(input.value))
      .filter((value) => Number.isSafeInteger(value) && value > 0);
    if (!rootIds.length) {
      toast("請至少選擇一個建立區域", true);
      return;
    }
    ui.thumbnailCacheStart.disabled = true;
    ui.thumbnailCacheStart.textContent = "計算範圍中…";
    try {
      const preflight = await api("/api/thumbnail-cache-jobs/preflight", {
        method: "POST",
        body: { root_ids: rootIds },
      });
      state.thumbnailCachePreflight = preflight;
      renderThumbnailCachePreflight(preflight);
      ui.thumbnailCachePreflightDialog.showModal();
    } catch (error) {
      toast(error.message, true);
    } finally {
      renderThumbnailCacheRoots();
    }
  }

  function renderThumbnailCachePreflight(preflight) {
    ui.thumbnailCachePreflightSummary.textContent = `${formatNumber(preflight.root_count)} 個來源，共 ${formatNumber(preflight.collection_count)} 本收藏；其中 ${formatNumber(preflight.requires_build)} 張縮圖需要建立或更新，${formatNumber(preflight.ready)} 張已有有效快取。`;
    ui.thumbnailCachePreflightRoots.replaceChildren();
    const rootIds = new Set(preflight.root_ids || []);
    state.settingsRoots.filter((root) => rootIds.has(root.id)).forEach((root) => {
      const item = el("li", "");
      item.append(el("strong", "", root.label), el("code", "", root.path));
      ui.thumbnailCachePreflightRoots.append(item);
    });
    ui.thumbnailCacheConfirm.textContent = preflight.requires_build ? `開始建立 ${formatNumber(preflight.requires_build)} 張` : "確認範圍";
  }

  async function confirmThumbnailCacheJob(event) {
    event.preventDefault();
    const preflight = state.thumbnailCachePreflight;
    if (!preflight) return;
    ui.thumbnailCacheConfirm.disabled = true;
    ui.thumbnailCacheConfirm.textContent = "啟動中…";
    try {
      const job = await api("/api/thumbnail-cache-jobs", {
        method: "POST",
        body: { root_ids: preflight.root_ids },
      });
      state.thumbnailCachePreflight = null;
      ui.thumbnailCachePreflightDialog.close();
      updateThumbnailCacheJob(job, { announce: false });
      renderThumbnailCacheRoots();
      scheduleThumbnailCachePolling();
      if (job.status === "running") toast(`已開始檢查並建立 ${formatNumber(job.total)} 張快取縮圖；可離開此頁`);
      else if (job.status === "completed_with_errors") toast(`快取縮圖已部分完成：${formatNumber(job.failed)} 張失敗`, true);
      else toast(job.total ? "所選範圍的快取縮圖皆已備妥" : "所選範圍沒有需要建立的收藏");
    } catch (error) {
      toast(error.message, true);
    } finally {
      ui.thumbnailCacheConfirm.disabled = false;
      if (state.thumbnailCachePreflight) renderThumbnailCachePreflight(state.thumbnailCachePreflight);
    }
  }

  async function openThumbnailCacheFailures() {
    const job = state.thumbnailCacheJob;
    if (!job?.failed) return;
    if (state.selectedIds.size > 0 && !confirmSelectionClear()) return;
    try {
      const failures = await api("/api/thumbnail-cache-jobs/current/failures");
      if (failures.job_id !== job.id) {
        toast("快取縮圖工作已更新，請重新整理狀態後再試", true);
        return;
      }
      if (!failures.items.length) {
        toast("失敗收藏已不存在或無法載入", true);
        return;
      }
      clearSelection();
      failures.items.forEach((collection) => {
        state.selectedIds.add(collection.id);
        state.selectedRecords.set(collection.id, collection);
      });
      state.selectionContext = "thumbnail_failures";
      state.filters = {};
      state.filterTags = [];
      state.libraryDataKey = "";
      state.libraryRouteHash = "#library";
      state.libraryFocusId = null;
      state.libraryRestorePage = 1;
      state.libraryLoaded = false;
      syncFilterDraftFromApplied();
      updateFilterCount();
      updateLibraryNavHref();
      updateSelectionUI();
      setActivityPanelOpen(false);
      location.hash = "workbench";
      if (failures.missing_collection_ids.length) {
        toast(`${formatNumber(failures.missing_collection_ids.length)} 筆失敗收藏已不在目前目錄中`, true);
      }
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function retryThumbnailCacheFailures() {
    if (state.thumbnailCacheRetrying) return;
    state.thumbnailCacheRetrying = true;
    renderThumbnailCacheProgress();
    renderThumbnailCacheRoots();
    try {
      const job = await api("/api/thumbnail-cache-jobs/current/retry-failures", { method: "POST" });
      updateThumbnailCacheJob(job, { announce: false });
      scheduleThumbnailCachePolling();
      setActivityPanelOpen(false);
      toast(`已將 ${formatNumber(job.total)} 張失敗縮圖重新排入；已完成項目不會回滾`);
    } catch (error) {
      toast(error.message, true);
    } finally {
      state.thumbnailCacheRetrying = false;
      renderThumbnailCacheProgress();
      renderThumbnailCacheRoots();
    }
  }

  function updateThumbnailCacheJob(job, { announce = true } = {}) {
    const previous = state.thumbnailCacheJob;
    state.thumbnailCacheJob = job;
    renderThumbnailCacheProgress();
    if (state.route === "settings" && previous && job && (previous.id !== job.id || previous.status !== job.status)) {
      renderThumbnailCacheRoots();
    }
    if (announce && previous?.id === job?.id && previous.status === "running" && job.status !== "running") {
      toast(job.status === "completed_with_errors"
        ? `快取縮圖已部分完成：${formatNumber(job.failed)} 張失敗`
        : "快取縮圖建立完成", job.status === "completed_with_errors");
    }
    renderActivityCenter();
  }

  function scheduleThumbnailCachePolling() {
    stopThumbnailCachePolling();
    if (state.route !== "settings" || state.thumbnailCacheJob?.status !== "running") return;
    state.thumbnailCacheTimer = window.setTimeout(async () => {
      state.thumbnailCacheTimer = null;
      try {
        const cacheJobs = await api("/api/thumbnail-cache-jobs/current");
        updateThumbnailCacheJob(cacheJobs.job);
      } catch (_) {
        // The global activity monitor reports service availability.
      }
      scheduleThumbnailCachePolling();
    }, 1000);
  }

  function stopThumbnailCachePolling() {
    if (state.thumbnailCacheTimer != null) window.clearTimeout(state.thumbnailCacheTimer);
    state.thumbnailCacheTimer = null;
  }

  function formatProgressPercent(value) {
    const numeric = Number(value) || 0;
    return `${Number.isInteger(numeric) ? numeric.toFixed(0) : numeric.toFixed(1)}%`;
  }

  function formatThumbnailEta(seconds) {
    if (seconds == null) return "正在估算剩餘時間";
    if (seconds <= 0) return "即將完成";
    if (seconds < 60) return `約 ${Math.max(1, Math.ceil(seconds))} 秒`;
    if (seconds < 3600) return `約 ${Math.ceil(seconds / 60)} 分鐘`;
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.ceil((seconds % 3600) / 60);
    return `約 ${hours} 小時${minutes ? ` ${minutes} 分鐘` : ""}`;
  }

  function formatDuration(seconds) {
    const total = Math.max(0, Math.floor(Number(seconds) || 0));
    if (total < 60) return `${total} 秒`;
    if (total < 3600) return `${Math.floor(total / 60)} 分 ${total % 60} 秒`;
    const hours = Math.floor(total / 3600);
    const minutes = Math.floor((total % 3600) / 60);
    return `${hours} 小時${minutes ? ` ${minutes} 分` : ""}`;
  }

  async function saveSettings(event) {
    event.preventDefault();
    const form = new FormData(ui.settingsForm);
    const submit = ui.settingsForm.querySelector('[type="submit"]');
    submit.disabled = true;
    try {
      const settingsSnapshot = state.settingsSnapshot;
      const settings = await api("/api/settings", {
        method: "PUT",
        body: {
          viewer_path: settingsSnapshot?.overrides.viewer_path
            ? settingsSnapshot.saved_viewer_path
            : String(form.get("viewer_path") || "").trim(),
          thumb_size: settingsSnapshot?.overrides.thumb_size
            ? settingsSnapshot.saved_thumb_size
            : String(form.get("thumb_size") || "").trim(),
          thumb_quality: settingsSnapshot?.overrides.thumb_quality
            ? settingsSnapshot.saved_thumb_quality
            : Number(form.get("thumb_quality")),
        },
      });
      const requeued = settings.thumbnails_requeued || 0;
      toast(requeued ? `設定已儲存，${formatNumber(requeued)} 張縮圖已排入重建` : "設定已儲存");
      await loadSettingsPage();
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
    }
  }

  function renderRoots(roots) {
    ui.rootList.replaceChildren();
    if (!roots.length) {
      ui.rootList.append(el("li", "root-empty", "尚未登記資料夾來源。"));
      return;
    }
    roots.forEach((root) => {
      const item = el("li", `root-item${root.active ? "" : " inactive"}`);
      const purposeLabel = root.source === "downloads" ? "新收藏" : "典藏庫";
      item.append(
        el("strong", "root-name", root.label),
        el("code", "root-path", root.path),
        el("span", `root-purpose ${root.source}`, purposeLabel),
        el("span", `root-status ${root.active ? "active" : "inactive"}`, root.active ? "已啟用" : "已停用"),
      );
      const actions = el("div", "root-actions");
      const edit = el("button", "text-button", "編輯");
      edit.type = "button";
      edit.addEventListener("click", () => openEditRoot(root));
      actions.append(edit);
      if (root.active) {
        const deactivate = el("button", "text-button danger-text", "停用");
        deactivate.type = "button";
        deactivate.addEventListener("click", () => deactivateRoot(root));
        actions.append(deactivate);
      } else {
        const activate = el("button", "secondary-button", "重新啟用");
        activate.type = "button";
        activate.addEventListener("click", () => reactivateRoot(root));
        actions.append(activate);
      }
      item.append(actions);
      ui.rootList.append(item);
    });
  }

  async function registerRoot(event) {
    event.preventDefault();
    const form = new FormData(ui.rootForm);
    const submit = ui.rootForm.querySelector('[type="submit"]');
    submit.disabled = true;
    try {
      const root = await api("/api/library-roots", {
        method: "POST",
        body: {
          label: String(form.get("label") || "").trim(),
          path: String(form.get("path") || "").trim(),
          source: String(form.get("source") || "downloads"),
        },
      });
      ui.rootForm.reset();
      invalidateDerivedData({ library: true });
      state.rootsNeedScan = true;
      toast(`已登記資料夾「${root.label}」；請重新掃描`);
      await loadSettingsPage();
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
    }
  }

  function openEditRoot(root) {
    const form = ui.editRootForm;
    form.elements.root_id.value = root.id;
    form.elements.label.value = root.label;
    form.elements.path.value = root.path;
    form.elements.source.value = root.source;
    form.dataset.originalPath = root.path;
    form.dataset.originalSource = root.source;
    ui.editRootDialog.showModal();
    form.elements.label.focus();
  }

  async function saveEditedRoot(event) {
    event.preventDefault();
    const form = new FormData(ui.editRootForm);
    const submit = ui.editRootForm.querySelector('[type="submit"]');
    const rootId = Number(form.get("root_id"));
    const path = String(form.get("path") || "").trim();
    const source = String(form.get("source") || "downloads");
    const requiresScan = path !== ui.editRootForm.dataset.originalPath || source !== ui.editRootForm.dataset.originalSource;
    submit.disabled = true;
    try {
      const root = await api(`/api/library-roots/${rootId}`, {
        method: "PATCH",
        body: {
          label: String(form.get("label") || "").trim(),
          path,
          source,
        },
      });
      ui.editRootDialog.close();
      invalidateDerivedData({ library: true });
      if (requiresScan) state.rootsNeedScan = true;
      toast(requiresScan ? `已更新「${root.label}」；請重新掃描` : `已更新「${root.label}」`);
      await loadSettingsPage();
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
    }
  }

  async function reactivateRoot(root) {
    try {
      const activated = await api(`/api/library-roots/${root.id}/activate`, { method: "POST" });
      invalidateDerivedData({ library: true });
      state.rootsNeedScan = true;
      toast(`已重新啟用「${activated.label}」；請重新掃描`);
      await loadSettingsPage();
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function deactivateRoot(root) {
    if (!window.confirm(`停用資料夾來源「${root.label}」？這不會刪除磁碟檔案或既有收藏紀錄。`)) return;
    try {
      await api(`/api/library-roots/${root.id}`, { method: "DELETE" });
      invalidateDerivedData({ library: true });
      state.rootsNeedScan = true;
      toast(`已停用「${root.label}」；請重新掃描`);
      await loadSettingsPage();
    } catch (error) {
      toast(error.message, true);
    }
  }

  function startScan() {
    return previewScan(ui.scanButton, "預覽中…", false);
  }

  function scanEmptyLibrary() {
    return previewScan(ui.emptyPrimary, "預覽首次掃描…", true);
  }

  async function previewScan(button, runningLabel, reloadLibrary) {
    if (state.selectedIds.size > 0 && !confirmSelectionClear()) return;
    if (state.selectedIds.size > 0) clearSelection();
    const original = button.textContent;
    button.disabled = true;
    button.textContent = runningLabel;
    state.scanRequest = { button, original, reloadLibrary };
    try {
      const preflight = await api("/api/scans/preflight", { method: "POST" });
      state.scanPreflight = preflight;
      renderScanPreflight(preflight);
      ui.scanPreflightDialog.showModal();
    } catch (error) {
      state.scanRequest = null;
      toast(error.message, true);
    } finally {
      button.disabled = false;
      button.textContent = original;
    }
  }

  function renderScanPreflight(preflight) {
    const expectation = preflight.expectation || {};
    const issueCount = (preflight.issues || []).length;
    ui.scanPreflightSummary.textContent = `${formatNumber((preflight.roots || []).length)} 個來源 · 預計新增 ${formatNumber(expectation.new_collections || 0)} 本 · 已知 ${formatNumber(expectation.already_known || 0)} 本 · ${formatNumber(expectation.planned_renames || 0)} 本會改名 · ${formatNumber(expectation.normalization_warnings || 0)} 個改名警告 · ${formatNumber(expectation.possible_candidate_links || 0)} 組身分候選${issueCount ? ` · ${formatNumber(issueCount)} 個來源問題` : ""}`;
    ui.scanPreflightRenames.replaceChildren();
    ui.scanPreflightWarnings.replaceChildren();
    ui.scanPreflightTombstones.replaceChildren();

    (preflight.renames || []).forEach((rename) => {
      const item = el("li", "scan-change-item");
      const diff = el("div", "scan-rename-diff");
      diff.append(el("code", "", rename.before || ""), el("span", "", "→"), el("code", "", rename.after || ""));
      item.append(diff, el("small", "", "percent decode · parser 結構正規化"));
      ui.scanPreflightRenames.append(item);
    });
    const warnings = [
      ...(preflight.rename_warnings || []).map((warning) => ({ path: warning.path, message: warning.reason })),
      ...(preflight.issues || []).map((issue) => ({ path: issue.path, message: issue.message })),
    ];
    warnings.forEach((warning) => {
      const item = el("li", "scan-change-item");
      item.append(el("code", "", warning.path || "未指定路徑"), el("small", "", warning.message || "無法預先判定"));
      ui.scanPreflightWarnings.append(item);
    });
    (preflight.tombstone_candidates || []).forEach((candidate) => {
      const item = el("li", "scan-change-item");
      item.append(
        el("code", "", candidate.tombstone_path || ""),
        el("small", "", `可能與 ${candidate.candidate_path || "新收藏"} 形成同名候選`),
      );
      ui.scanPreflightTombstones.append(item);
    });
    ui.scanPreflightRenamesSection.hidden = !(preflight.renames || []).length;
    ui.scanPreflightWarningsSection.hidden = !warnings.length;
    ui.scanPreflightTombstonesSection.hidden = !(preflight.tombstone_candidates || []).length;
    ui.scanPreflightDetails.hidden = !(preflight.renames || []).length && !warnings.length && !(preflight.tombstone_candidates || []).length;
  }

  async function applyScanPreflight(event) {
    event.preventDefault();
    if (!state.scanPreflight || !state.scanRequest) return;
    const mode = new FormData(ui.scanPreflightForm).get("mode") || "apply_safe_renames";
    const request = state.scanRequest;
    ui.scanPreflightConfirm.disabled = true;
    ui.scanPreflightConfirm.textContent = "掃描中…";
    ui.scanPreflightDialog.close();
    state.activityScan = { id: null, status: "running", issues: [], message: "正在掃描資料夾來源", updatedAt: new Date().toISOString() };
    renderActivityCenter();
    try {
      const report = await api("/api/scans", {
        method: "POST",
        body: { mode, expected: state.scanPreflight.expectation },
      });
      const summary = report.summary;
      const prefix = report.status === "partial" ? "掃描部分完成" : "掃描完成";
      state.activityScan = scanActivity(report);
      state.rootsNeedScan = false;
      ui.rootRescanNote.hidden = true;
      const drift = summary.preflight_differences || [];
      toast(`${prefix}：新增 ${formatNumber(summary.added)}、略過 ${formatNumber(summary.skipped)}、問題 ${formatNumber(report.issues.length)}${drift.length ? `；${formatNumber(drift.length)} 項與預覽不同` : ""}`, report.status === "partial" || drift.length > 0);
      invalidateDerivedData({ library: true });
      state.libraryFocusId = null;
      state.libraryDataKey = null;
      if (request.reloadLibrary && state.route === "library") await loadCollections();
    } catch (error) {
      state.activityScan = { id: null, status: "failed", issues: [], message: error.message, updatedAt: new Date().toISOString() };
      toast(error.message, true);
    } finally {
      state.scanPreflight = null;
      state.scanRequest = null;
      ui.scanPreflightConfirm.disabled = false;
      ui.scanPreflightConfirm.textContent = "套用掃描";
      renderActivityCenter();
    }
  }

  function handleKeyboard(event) {
    const target = event.target;
    const isTyping = target instanceof HTMLInputElement || target instanceof HTMLSelectElement || target instanceof HTMLTextAreaElement || target?.isContentEditable;
    if (event.key === "Escape" && !ui.activityPanel.hidden) {
      event.preventDefault();
      setActivityPanelOpen(false);
      ui.activityTrigger.focus({ preventScroll: true });
      return;
    }
    if (event.key === "Escape" && ui.mobileDetailDialog.open && !document.querySelector("dialog[open]:not(#mobile-detail-dialog)")) {
      event.preventDefault();
      closeMobileDetail();
      return;
    }
    if (event.key === "Escape" && !ui.filterPanel.hidden && !isDialogOpen()) {
      event.preventDefault();
      requestFilterPanelClose({ restoreFocus: true });
      return;
    }
    if (event.key === "/" && !isTyping && !isDialogOpen()) {
      event.preventDefault();
      if (state.route !== "library") location.hash = state.libraryRouteHash;
      ui.searchInput.focus();
      return;
    }
    if (event.key === "?" && !isTyping && !isDialogOpen()) {
      event.preventDefault();
      byId("shortcuts-dialog").showModal();
      return;
    }
    if (state.route === "review" && !isTyping && !isDialogOpen() && !event.altKey && !event.ctrlKey && !event.metaKey) {
      const key = event.key.toLowerCase();
      if (["a", "r", "e", "s", "j", "k"].includes(key)) {
        event.preventDefault();
        if (key === "a" && !ui.reviewAccept.disabled) decideReviewCandidate("select");
        else if (key === "r" && !ui.reviewReject.disabled) decideReviewCandidate("reject");
        else if (key === "e" && !ui.reviewEdit.disabled) openReviewEditor();
        else if (key === "s" && !ui.reviewSkip.disabled) skipCurrentReviewItem();
        else if (key === "j") moveReviewPosition(1);
        else if (key === "k") moveReviewPosition(-1);
      }
      return;
    }
    if (state.route !== "library" || isTyping || isDialogOpen()) return;
    const isCollectionButton = Boolean(target.closest?.(".collection-item-button"));
    if (event.key === "Enter" && state.selected && (!target.closest("button, a") || isCollectionButton)) {
      event.preventDefault();
      launchSelected("read");
      return;
    }
    if (event.key === " " && state.selected && (!target.closest("button, a") || isCollectionButton)) {
      event.preventDefault();
      toggleCollectionSelection(state.selected, !state.selectedIds.has(state.selected.id));
      return;
    }
    if (!["j", "k", "J", "K"].includes(event.key)) return;
    event.preventDefault();
    moveLibraryFocus(event.key.toLowerCase() === "j" ? 1 : -1);
  }

  async function moveLibraryFocus(direction) {
    if (!state.items.length) return;
    const current = state.items.findIndex((item) => item.id === state.selected?.id);
    if (direction > 0 && current === state.items.length - 1 && state.page < state.totalPages) {
      const firstNewIndex = state.items.length;
      if (await loadMoreCollections() && state.items[firstNewIndex]) {
        selectCollection(state.items[firstNewIndex], { focus: true });
      }
      return;
    }
    const next = Math.min(state.items.length - 1, Math.max(0, (current < 0 ? 0 : current) + direction));
    selectCollection(state.items[next], { focus: true });
  }

  function isDialogOpen() {
    return Boolean(document.querySelector("dialog[open]"));
  }

  function invalidateDerivedData({ library = false } = {}) {
    state.statsLoaded = false;
    state.statsData = null;
    state.shelfLoaded = false;
    state.shelfData = null;
    state.savedViewsLoaded = false;
    if (library) state.libraryLoaded = false;
  }

  async function api(path, options = {}) {
    const headers = { Accept: "application/json", ...(options.headers || {}) };
    const init = { method: options.method || "GET", headers };
    if (options.body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(options.body);
    }
    let response;
    try {
      response = await fetch(path, init);
    } catch (_) {
      throw new Error("無法連線至 Rust 本機服務，請確認程式仍在執行");
    }
    const contentType = response.headers.get("content-type") || "";
    const data = contentType.includes("application/json") ? await response.json() : null;
    if (!response.ok) {
      const error = new Error(data?.error?.message || `要求失敗（HTTP ${response.status}）`);
      error.code = data?.error?.code;
      error.status = response.status;
      throw error;
    }
    return data;
  }

  function toast(message, isError = false) {
    const notice = el("div", `toast${isError ? " error" : ""}`, message);
    notice.setAttribute("role", isError ? "alert" : "status");
    ui.toastRegion.append(notice);
    window.setTimeout(() => notice.remove(), 4800);
  }

  function displayTitle(collection) {
    return collection.title || collection.filename || `收藏 #${collection.id}`;
  }

  function bindThumbnail(image, collectionId, requestEpoch = nextThumbnailRequestEpoch()) {
    unbindThumbnail(image);
    const binding = { collectionId: Number(collectionId), requestEpoch, active: false, statusLabel: null };
    thumbnailBindings.set(image, binding);
    image.dataset.thumbnailCollectionId = String(binding.collectionId);
    image.dataset.thumbnailStatus = "pending";
    image.setAttribute("aria-busy", "true");
    image.removeAttribute("src");
    window.queueMicrotask(() => {
      if (thumbnailBindings.get(image) === binding) setThumbnailElementStatus(image, "pending", null, false);
    });
    if (thumbnailObserver) thumbnailObserver.observe(image);
    else window.queueMicrotask(() => activateThumbnailElement(image));
  }

  function activateThumbnailElement(image) {
    const binding = thumbnailBindings.get(image);
    if (!binding || binding.active || !image.isConnected) return;
    binding.active = true;
    let tracker = thumbnailTrackers.get(binding.collectionId);
    if (!tracker) {
      tracker = {
        collectionId: binding.collectionId,
        elements: new Set(),
        status: "pending",
        errorKind: null,
        readyUrl: null,
        terminal: false,
        pollAttempt: 0,
        networkAttempt: 0,
        priority: 0,
        requestQueued: false,
        requestInFlight: false,
        requestNumber: 0,
        controller: null,
        timer: null,
      };
      thumbnailTrackers.set(binding.collectionId, tracker);
    }
    tracker.elements.add(image);
    tracker.priority = Math.max(tracker.priority, binding.requestEpoch + thumbnailPriority(image));
    if (tracker.readyUrl) {
      showReadyThumbnail(image, tracker.readyUrl);
      return;
    }
    setThumbnailElementStatus(image, tracker.status, tracker.errorKind, tracker.terminal);
    requestTrackedThumbnail(tracker);
  }

  function unbindThumbnail(image) {
    const binding = thumbnailBindings.get(image);
    if (!binding) return;
    thumbnailObserver?.unobserve(image);
    if (binding.active) {
      const tracker = thumbnailTrackers.get(binding.collectionId);
      tracker?.elements.delete(image);
      if (tracker && tracker.elements.size === 0) disposeThumbnailTracker(tracker);
    }
    thumbnailBindings.delete(image);
    binding.statusLabel?.remove();
    delete image.dataset.thumbnailCollectionId;
    delete image.dataset.thumbnailStatus;
    image.removeAttribute("aria-describedby");
    image.removeAttribute("aria-busy");
    image.removeAttribute("title");
    image.removeAttribute("src");
  }

  function unbindThumbnailsWithin(root) {
    root?.querySelectorAll("[data-thumbnail-collection-id]").forEach(unbindThumbnail);
  }

  function pauseThumbnailsWithin(root) {
    root?.querySelectorAll("[data-thumbnail-collection-id]").forEach((image) => {
      const binding = thumbnailBindings.get(image);
      if (!binding || !binding.active || image.dataset.thumbnailStatus === "ready") return;
      thumbnailObserver?.unobserve(image);
      binding.active = false;
      const tracker = thumbnailTrackers.get(binding.collectionId);
      tracker?.elements.delete(image);
      if (tracker && tracker.elements.size === 0) disposeThumbnailTracker(tracker);
    });
  }

  function resumeThumbnailsWithin(root) {
    root?.querySelectorAll("[data-thumbnail-collection-id]").forEach((image) => {
      const binding = thumbnailBindings.get(image);
      if (!binding || binding.active || image.dataset.thumbnailStatus === "ready") return;
      if (thumbnailObserver) thumbnailObserver.observe(image);
      else activateThumbnailElement(image);
    });
  }

  function thumbnailPriority(image) {
    const bounds = image.getBoundingClientRect();
    return bounds.bottom > 0 && bounds.top < window.innerHeight && bounds.right > 0 && bounds.left < window.innerWidth ? 1 : 0;
  }

  function nextThumbnailRequestEpoch() {
    lastThumbnailRequestEpoch = Math.max(lastThumbnailRequestEpoch + 2, Date.now() * 2);
    return lastThumbnailRequestEpoch;
  }

  function requestTrackedThumbnail(tracker) {
    if (
      tracker.requestQueued ||
      tracker.requestInFlight ||
      tracker.terminal ||
      tracker.elements.size === 0 ||
      thumbnailTrackers.get(tracker.collectionId) !== tracker
    ) return;

    tracker.requestQueued = true;
    thumbnailRequestQueue.push(tracker);
    drainThumbnailRequestQueue();
  }

  function drainThumbnailRequestQueue() {
    while (thumbnailRequestsInFlight < THUMBNAIL_REQUEST_CONCURRENCY && thumbnailRequestQueue.length) {
      thumbnailRequestQueue.sort((left, right) => right.priority - left.priority);
      const tracker = thumbnailRequestQueue.shift();
      tracker.requestQueued = false;
      if (tracker.terminal || tracker.elements.size === 0 || thumbnailTrackers.get(tracker.collectionId) !== tracker) continue;
      thumbnailRequestsInFlight += 1;
      performTrackedThumbnailRequest(tracker).finally(() => {
        thumbnailRequestsInFlight -= 1;
        drainThumbnailRequestQueue();
      });
    }
  }

  async function performTrackedThumbnailRequest(tracker) {
    tracker.requestInFlight = true;
    const requestNumber = ++tracker.requestNumber;
    const controller = new AbortController();
    tracker.controller = controller;
    try {
      const response = await fetch(thumbnailUrl(tracker.collectionId, tracker.priority), {
        cache: "no-store",
        signal: controller.signal,
      });
      if (thumbnailTrackers.get(tracker.collectionId) !== tracker || requestNumber !== tracker.requestNumber) return;

      const status = response.headers.get("x-thumbnail-status") || (response.status === 200 ? "ready" : "pending");
      const errorKind = response.headers.get("x-thumbnail-error-kind");
      const nextRetryAt = response.headers.get("x-thumbnail-next-retry-at");

      if (response.status === 200 && status === "ready") {
        const thumbnail = await response.blob();
        const readyUrl = await blobAsDataUrl(thumbnail);
        if (thumbnailTrackers.get(tracker.collectionId) !== tracker || requestNumber !== tracker.requestNumber) return;
        tracker.status = "ready";
        tracker.readyUrl = readyUrl;
        tracker.terminal = true;
        tracker.elements.forEach((image) => showReadyThumbnail(image, tracker.readyUrl));
        return;
      }
      if (response.body) await response.body.cancel();
      if (response.status !== 202) throw new Error(`thumbnail HTTP ${response.status}`);

      tracker.status = status;
      tracker.errorKind = errorKind;
      tracker.elements.forEach((image) => setThumbnailElementStatus(image, status, errorKind, false));
      if (status === "failed" && !nextRetryAt) {
        tracker.terminal = true;
        tracker.elements.forEach((image) => setThumbnailElementStatus(image, status, errorKind, true));
        return;
      }

      tracker.networkAttempt = 0;
      const retryTime = nextRetryAt ? new Date(nextRetryAt).getTime() : Number.NaN;
      const delay = Number.isFinite(retryTime)
        ? Math.max(1000, retryTime - Date.now())
        : THUMBNAIL_POLL_DELAYS[Math.min(tracker.pollAttempt++, THUMBNAIL_POLL_DELAYS.length - 1)];
      scheduleThumbnailRequest(tracker, delay);
    } catch (error) {
      if (error?.name === "AbortError" || thumbnailTrackers.get(tracker.collectionId) !== tracker) return;
      tracker.status = "pending";
      tracker.elements.forEach((image) => setThumbnailElementStatus(image, "pending", null, false));
      const delay = THUMBNAIL_NETWORK_DELAYS[Math.min(tracker.networkAttempt++, THUMBNAIL_NETWORK_DELAYS.length - 1)];
      scheduleThumbnailRequest(tracker, delay);
    } finally {
      if (requestNumber === tracker.requestNumber) {
        tracker.requestInFlight = false;
        tracker.controller = null;
      }
    }
  }

  function scheduleThumbnailRequest(tracker, delay) {
    if (tracker.timer) window.clearTimeout(tracker.timer);
    const jitter = (tracker.collectionId % 7) * 80;
    tracker.timer = window.setTimeout(() => {
      tracker.timer = null;
      requestTrackedThumbnail(tracker);
    }, Math.min(delay + jitter, 2147483647));
  }

  function setThumbnailElementStatus(image, status, errorKind, terminal) {
    image.dataset.thumbnailStatus = terminal ? "failed" : status;
    image.setAttribute("aria-busy", String(!terminal));
    const label = ensureThumbnailStatusLabel(image);
    if (label) {
      label.hidden = false;
      label.className = `thumbnail-state-label ${terminal ? "failed" : "pending"}`;
      label.textContent = terminal ? "縮圖失敗" : status === "failed" ? "縮圖重試中" : "縮圖產生中";
    }
    if (terminal) {
      image.title = `縮圖無法產生${errorKind ? `（${errorKind}）` : ""}；可從詳細資料手動重建`;
    } else {
      image.removeAttribute("title");
    }
    if (terminal) {
      const before = state.activityThumbnailFailures.size;
      state.activityThumbnailFailures.add(Number(image.dataset.thumbnailCollectionId));
      if (state.activityThumbnailFailures.size !== before) renderActivityCenter();
    }
    if (image === ui.detailCover) renderDataQualitySummary();
  }

  function showReadyThumbnail(image, readyUrl) {
    const collectionId = Number(image.dataset.thumbnailCollectionId);
    image.dataset.thumbnailStatus = "ready";
    image.setAttribute("aria-busy", "false");
    image.removeAttribute("title");
    image.loading = "eager";
    image.src = readyUrl;
    const label = ensureThumbnailStatusLabel(image);
    if (label) {
      label.hidden = true;
      label.textContent = "";
    }
    if (state.activityThumbnailFailures.delete(collectionId)) renderActivityCenter();
    if (image === ui.detailCover) renderDataQualitySummary();
  }

  function ensureThumbnailStatusLabel(image) {
    if (!image.matches(".item-cover, .shelf-cover, #detail-cover, .selected-cover, .candidate-item > img")) return null;
    const binding = thumbnailBindings.get(image);
    if (!binding || !image.parentElement) return null;
    if (!binding.statusLabel) {
      const label = el("span", "thumbnail-state-label");
      label.id = `thumbnail-state-${++lastThumbnailStatusId}`;
      image.insertAdjacentElement("afterend", label);
      image.setAttribute("aria-describedby", label.id);
      binding.statusLabel = label;
    }
    return binding.statusLabel;
  }

  function blobAsDataUrl(blob) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.addEventListener("load", () => resolve(reader.result), { once: true });
      reader.addEventListener("error", () => reject(reader.error || new Error("縮圖讀取失敗")), { once: true });
      reader.readAsDataURL(blob);
    });
  }

  function disposeThumbnailTracker(tracker) {
    if (thumbnailTrackers.get(tracker.collectionId) !== tracker) return;
    if (tracker.timer) window.clearTimeout(tracker.timer);
    tracker.controller?.abort();
    thumbnailTrackers.delete(tracker.collectionId);
  }

  function restartThumbnailCollection(collectionId) {
    const tracker = thumbnailTrackers.get(Number(collectionId));
    if (tracker) disposeThumbnailTracker(tracker);
    document.querySelectorAll(`[data-thumbnail-collection-id="${Number(collectionId)}"]`).forEach((image) => {
      bindThumbnail(image, collectionId);
    });
  }

  function thumbnailUrl(id, priority) {
    const query = priority ? `?priority=${encodeURIComponent(priority)}` : "";
    return `/api/collections/${id}/thumbnail${query}`;
  }

  function formatNumber(value) {
    return new Intl.NumberFormat("zh-TW").format(value || 0);
  }

  function formatRecentTime(value) {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return "";
    return new Intl.DateTimeFormat("zh-TW", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(date);
  }

  function formatMetadataTime(value) {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return "時間不明";
    return new Intl.DateTimeFormat("zh-TW", { year: "numeric", month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(date);
  }

  function formatPercent(value) {
    const number = Number(value);
    if (!Number.isFinite(number)) return "—";
    return new Intl.NumberFormat("zh-TW", { style: "percent", maximumFractionDigits: 0 }).format(number);
  }

  function el(tag, className = "", text = "") {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== "") node.textContent = String(text);
    return node;
  }

  function byId(id) {
    return document.getElementById(id);
  }

  function readStorage(key, fallback) {
    try {
      const value = localStorage.getItem(key);
      return value == null ? fallback : JSON.parse(value);
    } catch (_) {
      return fallback;
    }
  }

  function writeStorage(key, value) {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch (_) {
      // The catalogue remains usable when browser storage is unavailable.
    }
  }
})();
