(() => {
  "use strict";

  const RECENT_KEY = "doujin-library.recent.v1";
  const LAYOUT_KEY = "doujin-library.layout.v1";
  const EXTERNAL_JOB_KEY = "doujin-library.external-jobs.v1";
  const RECENT_LIMIT = 20;
  const PER_PAGE = 96;
  const SHELF_LIMIT = 8;
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
    libraryFocusId: null,
    restoreLibraryContext: false,
    leavingLibraryContextCaptured: false,
    selectedIds: new Set(),
    selectedRecords: new Map(),
    layout: readStorage(LAYOUT_KEY, "grid"),
    recent: readStorage(RECENT_KEY, []),
    requestNumber: 0,
    statsLoaded: false,
    statsData: null,
    shelfLoaded: false,
    shelfData: null,
    libraryLoaded: false,
    libraryLoading: false,
    workbenchLoaded: false,
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
    serviceOnline: null,
    activityExternalJobs: new Map(),
    activityScan: null,
    activityThumbnailFailures: new Set(),
    lastBatchActivity: null,
    activityTimer: null,
    activitySignature: null,
  };

  if (!Array.isArray(state.recent)) state.recent = [];
  if (!['list', 'grid'].includes(state.layout)) state.layout = 'list';
  if (!state.externalJobRefs || typeof state.externalJobRefs !== "object" || Array.isArray(state.externalJobRefs)) state.externalJobRefs = {};

  const ui = {};
  const mobileDetailMedia = window.matchMedia("(max-width: 899px)");
  const facetControllers = new Map();
  const thumbnailBindings = new WeakMap();
  const thumbnailTrackers = new Map();
  const thumbnailRequestQueue = [];
  let thumbnailRequestsInFlight = 0;
  let lastThumbnailRequestEpoch = 0;
  let lastThumbnailStatusId = 0;
  let mobileDetailReturnId = null;
  let mobileDetailScrollPosition = 0;
  let mobileDetailRestoreFocus = true;
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
      searchForm: byId("search-form"),
      searchInput: byId("search-input"),
      filterPanel: byId("filter-panel"),
      filterToggle: byId("filter-toggle"),
      activeFilterCount: byId("active-filter-count"),
      activeFilterChips: byId("active-filter-chips"),
      filterTagChips: byId("filter-tag-chips"),
      results: byId("collection-results"),
      loading: byId("library-loading"),
      empty: byId("library-empty"),
      resultSummary: byId("result-summary"),
      pagination: byId("pagination"),
      previousPage: byId("previous-page"),
      nextPage: byId("next-page"),
      pageLabel: byId("page-label"),
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
      rootList: byId("root-list"),
      rootForm: byId("root-form"),
      scanButton: byId("scan-button"),
      toastRegion: byId("toast-region"),
      selectionRail: byId("selection-rail"),
      selectionCount: byId("selection-count"),
      workbenchCount: byId("workbench-count"),
      workbenchSelectionSummary: byId("workbench-selection-summary"),
      selectedCollectionList: byId("selected-collection-list"),
      selectionEmpty: byId("selection-empty"),
      batchTools: byId("batch-tools"),
      batchTagForm: byId("batch-tag-form"),
      batchMetadataForm: byId("batch-metadata-form"),
      batchResult: byId("batch-result"),
      batchResultSummary: byId("batch-result-summary"),
      batchResultItems: byId("batch-result-items"),
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
    renderRecent();
    setLayout(state.layout);
    routeFromHash();
    startActivityMonitoring();
  }

  function bindEvents() {
    initializeFacetComboboxes();
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
    ui.searchForm.addEventListener("submit", (event) => {
      event.preventDefault();
      state.page = 1;
      state.libraryFocusId = null;
      readFilters();
      setFilterPanelOpen(false);
      navigateLibrary();
    });
    ui.filterToggle.addEventListener("click", () => {
      setFilterPanelOpen(ui.filterPanel.hidden);
    });
    byId("close-filter-panel").addEventListener("click", () => setFilterPanelOpen(false, { restoreFocus: true }));
    document.addEventListener("pointerdown", (event) => {
      if (ui.filterPanel.hidden || ui.filterPanel.contains(event.target) || ui.filterToggle.contains(event.target)) return;
      setFilterPanelOpen(false);
    });
    byId("clear-filters").addEventListener("click", () => resetSearch(false));
    byId("empty-reset").addEventListener("click", () => resetSearch(true));
    ui.previousPage.addEventListener("click", () => changePage(state.page - 1));
    ui.nextPage.addEventListener("click", () => changePage(state.page + 1));
    document.querySelectorAll("[data-layout]").forEach((button) => {
      button.addEventListener("click", () => setLayout(button.dataset.layout));
    });
    byId("read-button").addEventListener("click", () => launchSelected("read"));
    byId("open-button").addEventListener("click", () => launchSelected("open"));
    byId("edit-metadata-button").addEventListener("click", openMetadataDialog);
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
    initializeShelfScrollControls();
    byId("clear-recent").addEventListener("click", clearRecent);
    ui.metadataField.addEventListener("change", syncMetadataEditor);
    ui.metadataForm.addEventListener("submit", saveMetadata);
    byId("clear-manual-button").addEventListener("click", clearManualMetadata);
    ui.settingsForm.addEventListener("submit", saveSettings);
    ui.rootForm.addEventListener("submit", registerRoot);
    ui.scanButton.addEventListener("click", startScan);
    byId("select-page").addEventListener("click", selectCurrentPage);
    byId("invert-page").addEventListener("click", invertCurrentPageSelection);
    byId("clear-selection").addEventListener("click", clearSelection);
    ui.batchTagForm.addEventListener("submit", batchAddTag);
    ui.batchMetadataForm.elements.field.addEventListener("change", syncBatchMetadataField);
    ui.batchMetadataForm.addEventListener("submit", batchSetMetadata);
    byId("prepare-move").addEventListener("click", prepareMove);
    ui.moveForm.addEventListener("submit", executeMove);
    byId("prepare-delete").addEventListener("click", prepareDelete);
    ui.deleteForm.addEventListener("change", syncDeleteMode);
    ui.deleteForm.addEventListener("input", syncDeleteMode);
    ui.deleteForm.addEventListener("submit", executeDelete);
    byId("refresh-candidates").addEventListener("click", loadTombstoneCandidates);
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
    document.addEventListener("keydown", handleKeyboard);
  }

  function setFilterPanelOpen(open, { restoreFocus = false } = {}) {
    ui.filterPanel.hidden = !open;
    ui.filterToggle.setAttribute("aria-expanded", String(open));
    if (open) ui.filterPanel.querySelector("select, input")?.focus();
    else {
      closeAllFacetOptions();
      if (restoreFocus) ui.filterToggle.focus();
    }
  }

  function routeFromHash() {
    const previousRoute = state.route;
    const parsedRoute = parseRouteHash();
    const route = parsedRoute.route;
    const nextRoute = ["shelf", "library", "workbench", "stats", "settings"].includes(route) ? route : "shelf";
    if (previousRoute === "library" && nextRoute !== "library") {
      if (!state.leavingLibraryContextCaptured) rememberLibraryContext();
      state.leavingLibraryContextCaptured = false;
    }
    let libraryNeedsLoad = false;
    if (nextRoute === "library") {
      const decoded = decodeLibraryParams(parsedRoute.params);
      const dataChanged = state.libraryDataKey !== decoded.dataKey;
      libraryNeedsLoad = dataChanged || !state.libraryLoaded;
      if (libraryNeedsLoad && state.selectedIds.size > 0 && !confirmSelectionClear()) {
        const rollbackHash = previousRoute === "library" ? state.libraryRouteHash : `#${previousRoute}`;
        history.replaceState(null, "", rollbackHash);
        if (previousRoute === "library") {
          applyDecodedLibraryState(decodeLibraryParams(parseRouteHash().params));
        }
        return;
      }
      if (dataChanged) state.libraryScrollY = 0;
      applyDecodedLibraryState(decoded);
      state.libraryRouteHash = location.hash || "#library";
      updateLibraryNavHref();
      state.restoreLibraryContext = previousRoute !== "library" || libraryNeedsLoad;
    }
    state.route = nextRoute;
    if (state.route !== "library") closeMobileDetail({ restoreFocus: false });
    if (state.route !== "library" && !ui.filterPanel.hidden) setFilterPanelOpen(false);
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
    if (state.route === "library" && libraryNeedsLoad) loadCollections();
    else if (state.route === "library" && state.restoreLibraryContext) restoreLibraryWorkContext();
    if (state.route === "workbench") loadWorkbench();
    if (state.route === "stats") loadStats();
    if (state.route === "settings") loadSettingsPage();
    if (state.route !== "library") window.scrollTo({ top: 0, behavior: "auto" });
    document.title = `${routeTitle(state.route)}｜私藏編目室`;
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
    const page = Math.max(1, Number.parseInt(params.get("page") || "1", 10) || 1);
    const focusId = Number.parseInt(params.get("focus") || "", 10);
    const dataParams = libraryParams(values, page, null);
    return { values, tags, page, focusId: Number.isSafeInteger(focusId) && focusId > 0 ? focusId : null, dataKey: dataParams.toString() };
  }

  function applyDecodedLibraryState(decoded) {
    ui.searchForm.reset();
    Object.entries(decoded.values).forEach(([name, value]) => {
      if (name === "tag") return;
      const control = ui.searchForm.elements[name];
      if (control) control.value = value;
    });
    state.filterTags = [...decoded.tags];
    state.filters = { ...decoded.values, ...(decoded.tags.length ? { tag: [...decoded.tags] } : {}) };
    state.page = decoded.page;
    state.libraryFocusId = decoded.focusId;
    state.libraryDataKey = decoded.dataKey;
    renderFilterTagChips();
    updateFilterCount();
  }

  function libraryParams(filters = state.filters, page = state.page, focusId = state.libraryFocusId) {
    const params = new URLSearchParams();
    if (filters.q) params.set("q", filters.q);
    FILTER_NAMES.forEach((name) => {
      const value = filters[name];
      if (Array.isArray(value)) value.forEach((entry) => params.append(name, entry));
      else if (value) params.set(name, value);
    });
    if (page > 1) params.set("page", String(page));
    if (focusId) params.set("focus", String(focusId));
    return params;
  }

  function libraryHash() {
    const query = libraryParams().toString();
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

  function confirmSelectionClear() {
    return window.confirm(`這會清除目前 ${formatNumber(state.selectedIds.size)} 筆批次選取。要繼續嗎？`);
  }

  function rememberLibraryContext() {
    state.libraryScrollY = window.scrollY;
    state.libraryFocusId = Number(document.activeElement?.dataset?.collectionId) || state.selected?.id || state.libraryFocusId;
  }

  function updateLibraryNavHref() {
    document.querySelector('[data-route="library"]')?.setAttribute("href", state.libraryRouteHash);
  }

  function restoreLibraryWorkContext() {
    state.restoreLibraryContext = false;
    requestAnimationFrame(() => {
      window.scrollTo({ top: state.libraryScrollY, behavior: "auto" });
      if (!state.libraryFocusId) return;
      document.querySelector(`[data-collection-id="${state.libraryFocusId}"]`)?.focus({ preventScroll: true });
    });
  }

  function routeTitle(route) {
    return { shelf: "書架", library: "全部藏書", workbench: "工作台", stats: "統計", settings: "設定" }[route];
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
    }

    renderActivityCenter();
    const active = state.activityScan?.status === "running"
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
    const batchFailures = state.lastBatchActivity?.failed || 0;
    const attentionCount = failedJobs.length + state.activityThumbnailFailures.size + Number(scanNeedsAttention) + batchFailures;
    const runningCount = activeJobs.length + Number(scanRunning);

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
      ui.activityList.append(activityItem(`scan ${scan.status}`, scan.status === "running" ? "重新掃描進行中" : "最近一次掃描", detail, scan.status === "partial" ? "部分完成" : scan.status === "failed" ? "失敗" : scan.status === "succeeded" ? "完成" : "進行中", "查看設定", () => {
        setActivityPanelOpen(false);
        location.hash = "settings";
      }));
    }
    [...activeJobs, ...failedJobs].slice(0, 8).forEach((job) => {
      const fields = job.fields.map((field) => METADATA_LABELS[field] || field).join("、");
      ui.activityList.append(activityItem(`external ${job.status}`, `外部資料搜尋 #${job.id}`, `${fields} · 收藏 #${job.collection_id}`, EXTERNAL_JOB_STATUS_LABELS[job.status] || job.status, "查看收藏", () => openActivityCollection(job.collection_id)));
    });
    if (state.activityThumbnailFailures.size) {
      ui.activityList.append(activityItem("thumbnail failed", "縮圖生成失敗", `${formatNumber(state.activityThumbnailFailures.size)} 冊需要從收藏詳細資料重建縮圖。`, "需要處理", "查看藏書", () => {
        setActivityPanelOpen(false);
        location.hash = state.libraryRouteHash;
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

    const signature = [state.serviceOnline, runningCount, attentionCount, state.activityScan?.status || "", ...activeJobs.map((job) => `${job.id}:${job.status}`), ...failedJobs.map((job) => `${job.id}:${job.status}`)].join("|");
    if (state.activitySignature != null && signature !== state.activitySignature) {
      ui.activityAnnouncer.textContent = summary;
    }
    state.activitySignature = signature;
  }

  function activityItem(className, title, detail, status, actionLabel, action) {
    const item = el("li", `activity-item ${className}`);
    const copy = el("div", "");
    copy.append(el("strong", "", title), el("p", "", detail));
    const badge = el("span", "activity-status", status);
    const button = el("button", "text-button", actionLabel);
    button.type = "button";
    button.addEventListener("click", action);
    item.append(copy, badge, button);
    return item;
  }

  async function openActivityCollection(collectionId) {
    try {
      const collection = await api(`/api/collections/${collectionId}`);
      setActivityPanelOpen(false);
      if (state.route !== "library") location.hash = state.libraryRouteHash;
      selectCollection(collection);
      if (mobileDetailMedia.matches) openMobileDetail();
    } catch (error) {
      toast(`無法開啟這筆收藏：${error.message}`, true);
    }
  }

  async function loadShelf() {
    if (state.shelfLoaded) return;
    ui.shelfLoading.hidden = false;
    ui.shelfContent.hidden = true;
    try {
      const stats = await api("/api/stats");
      const recent = await shelfCollectionPage();
      const downloads = await shelfCollectionPage({ source: "downloads" }, 1);
      const candidateData = await api("/api/tombstone-candidates");
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

  function openShelfBook(collection, filterName, filterValue) {
    resetSearch(false);
    if (filterName && filterValue) ui.searchForm.elements[filterName].value = filterValue;
    readFilters();
    state.selected = collection;
    state.libraryFocusId = collection.id;
    state.page = 1;
    navigateLibrary();
  }

  function showShelfFilter(name, value) {
    resetSearch(false);
    if (name && value && ui.searchForm.elements[name]) ui.searchForm.elements[name].value = value;
    readFilters();
    state.libraryFocusId = null;
    state.page = 1;
    navigateLibrary();
  }

  function readFilters() {
    const data = new FormData(ui.searchForm);
    state.filters = {};
    const query = String(data.get("q") || "").trim();
    if (query) state.filters.q = query;
    FILTER_NAMES.forEach((name) => {
      if (name === "tag") return;
      const value = String(data.get(name) || "").trim();
      if (value) state.filters[name] = value;
    });
    if (state.filterTags.length) state.filters.tag = [...state.filterTags];
    updateFilterCount();
  }

  function updateFilterCount() {
    const count = Object.values(state.filters).reduce(
      (total, value) => total + (Array.isArray(value) ? value.length : value ? 1 : 0),
      0,
    );
    ui.activeFilterCount.textContent = String(count);
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
    if (name === "tag") {
      removeFilterTag(value);
      return;
    }
    const control = ui.searchForm.elements[name];
    if (control) control.value = "";
    readFilters();
    state.libraryFocusId = null;
    state.page = 1;
    navigateLibrary();
  }

  function resetSearch(load) {
    ui.searchForm.reset();
    state.filterTags = [];
    renderFilterTagChips();
    state.filters = {};
    state.libraryFocusId = null;
    state.page = 1;
    updateFilterCount();
    if (load) navigateLibrary();
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
      input.addEventListener("blur", () => setTimeout(() => closeFacetOptions(controller), 0));
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
        item.addEventListener("pointerdown", (event) => {
          event.preventDefault();
          selectFacetOption(controller, index);
        });
        item.addEventListener("pointermove", () => setFacetActive(controller, index));
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
      addFilterTag(controller.input.value, true);
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
      addFilterTag(option.name, true);
      controller.input.value = "";
    } else {
      controller.input.value = option.name;
      readFilters();
      state.libraryFocusId = null;
      state.page = 1;
      navigateLibrary();
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

  function addFilterTag(value, load = true) {
    const tag = String(value || "").trim();
    if (!tag || state.filterTags.some((existing) => existing.toLocaleLowerCase() === tag.toLocaleLowerCase())) return;
    state.filterTags.push(tag);
    renderFilterTagChips();
    readFilters();
    state.libraryFocusId = null;
    state.page = 1;
    if (load) navigateLibrary();
  }

  function removeFilterTag(value, load = true) {
    state.filterTags = state.filterTags.filter((tag) => tag !== value);
    renderFilterTagChips();
    readFilters();
    state.libraryFocusId = null;
    state.page = 1;
    if (load) navigateLibrary();
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

  async function loadCollections() {
    clearSelection();
    state.libraryLoading = true;
    const requestNumber = ++state.requestNumber;
    ui.loading.hidden = false;
    ui.empty.hidden = true;
    ui.results.hidden = true;
    ui.pagination.hidden = true;
    const params = new URLSearchParams({ page: String(state.page), per_page: String(PER_PAGE) });
    Object.entries(state.filters).forEach(([name, value]) => {
      if (Array.isArray(value)) value.forEach((entry) => params.append(name, entry));
      else params.set(name, value);
    });
    try {
      const data = await api(`/api/collections?${params}`);
      if (requestNumber !== state.requestNumber) return;
      state.items = data.items;
      state.total = data.pagination.total;
      state.totalPages = data.pagination.total_pages;
      state.libraryLoaded = true;
      ui.loading.hidden = true;
      renderCollections();
      renderPagination();
      if (state.route === "library" && state.restoreLibraryContext) restoreLibraryWorkContext();
      setServiceState("online", "本機服務正常");
    } catch (error) {
      if (requestNumber !== state.requestNumber) return;
      ui.loading.hidden = true;
      ui.results.hidden = false;
      ui.results.replaceChildren();
      ui.resultSummary.textContent = "無法讀取收藏";
      setServiceState("offline", "要求失敗");
      toast(error.message, true);
    } finally {
      if (requestNumber === state.requestNumber) state.libraryLoading = false;
    }
  }

  function renderCollections() {
    unbindThumbnailsWithin(ui.results);
    ui.results.replaceChildren();
    ui.results.hidden = state.items.length === 0;
    ui.empty.hidden = state.items.length !== 0;
    updateLibrarySummary();

    const thumbnailRequestEpoch = nextThumbnailRequestEpoch();
    state.items.forEach((collection, offset) => {
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
      button.setAttribute("aria-label", `選取 ${displayTitle(collection)}`);
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
      const index = el("span", "item-index", String((state.page - 1) * PER_PAGE + offset + 1).padStart(4, "0"));
      button.append(cover, copy, index);
      item.append(selectionControl, button);
      ui.results.append(item);
    });

    const preferredId = state.libraryFocusId || state.selected?.id;
    const onPage = preferredId && state.items.find((item) => item.id === preferredId);
    if (onPage) selectCollection(onPage, false);
    else if (state.items[0] && !window.matchMedia("(max-width: 899px)").matches) selectCollection(state.items[0], false);
    else {
      state.libraryFocusId = null;
      clearDetail();
    }
    updateSelectionUI();
  }

  function renderPagination() {
    ui.pagination.hidden = state.totalPages <= 1;
    ui.previousPage.disabled = state.page <= 1;
    ui.nextPage.disabled = state.page >= state.totalPages;
    ui.pageLabel.textContent = `第 ${state.page} / ${state.totalPages || 1} 頁`;
  }

  function changePage(page) {
    if (page < 1 || page > state.totalPages || page === state.page) return;
    state.page = page;
    state.libraryFocusId = null;
    state.libraryScrollY = 0;
    navigateLibrary();
  }

  function setLayout(layout) {
    state.layout = layout === "grid" ? "grid" : "list";
    writeStorage(LAYOUT_KEY, state.layout);
    ui.results?.classList.toggle("layout-list", state.layout === "list");
    ui.results?.classList.toggle("layout-grid", state.layout === "grid");
    document.querySelectorAll("[data-layout]").forEach((button) => {
      button.setAttribute("aria-pressed", String(button.dataset.layout === state.layout));
    });
  }

  function selectCollection(collection, focus = false) {
    state.selected = collection;
    state.libraryFocusId = collection.id;
    document.querySelectorAll(".collection-item-button").forEach((button) => {
      button.setAttribute("aria-current", String(Number(button.dataset.collectionId) === collection.id));
    });
    renderDetail(collection);
    if (state.route === "library") navigateLibrary({ replace: true });
    if (focus) {
      document.querySelector(`[data-collection-id="${collection.id}"]`)?.focus({ preventScroll: true });
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

  function missingMetadataLabels(collection) {
    if (!collection) return [];
    return [
      ["標題", collection.title],
      ["社團", collection.circle],
      ["作者", collection.authors?.length],
      ["原作", collection.parody || collection.parody_raw],
      ["場次", collection.event],
      ["種類", collection.classification_top],
      ["版本", collection.is_dl != null],
    ].filter(([, value]) => !value).map(([label]) => label);
  }

  function renderDataQualitySummary() {
    if (!ui.dataQualitySummary || !ui.evidenceSummaryCount) return;
    const missing = missingMetadataLabels(state.selected);
    const assertions = (state.metadataHistory?.fields || []).flatMap((field) => field.assertions || []);
    const pending = assertions.filter((assertion) => assertion.status === "candidate").length;
    const externalStatus = state.externalJob?.status;
    const thumbnailFailed = ui.detailCover?.dataset.thumbnailStatus === "failed";
    const parts = [];
    if (missing.length) parts.push(`缺少 ${missing.length} 欄（${missing.join("、")}）`);
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
    const control = ui.searchForm.elements[name];
    if (name === "tag") addFilterTag(value, false);
    else if (control) control.value = value;
    else return;
    closeMobileDetail({ restoreFocus: false });
    readFilters();
    state.libraryFocusId = null;
    state.page = 1;
    navigateLibrary();
    toast(`已加入篩選：${value}`);
  }

  function updateLibrarySummary() {
    if (!ui.resultSummary) return;
    ui.resultSummary.textContent = state.total === 0
      ? "沒有結果"
      : `共 ${formatNumber(state.total)} 筆 · 已選 ${formatNumber(state.selectedIds.size)} 筆`;
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
      location.hash = "library";
      selectCollection(collection);
      ui.recentDialog.close();
      if (mobileDetailMedia.matches) openMobileDetail();
      else ui.detailPane.scrollIntoView({ behavior: "smooth", block: "start" });
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

  function openMetadataDialog() {
    if (!state.selected) return;
    ui.metadataField.value = "title";
    syncMetadataEditor();
    ui.metadataDialog.showModal();
  }

  function syncMetadataEditor() {
    const field = ui.metadataField.value;
    const collection = state.selected;
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
    if (!state.selected) return;
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
      const collection = await api(`/api/collections/${state.selected.id}/metadata/${field}`, {
        method: "PUT",
        body: { value },
      });
      replaceSelected(collection);
      invalidateDerivedData();
      if (ui.metadataEvidence.open) loadMetadataEvidence(true);
      ui.metadataDialog.close();
      toast(`已儲存${METADATA_LABELS[field]}的手動值`);
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
    }
  }

  async function clearManualMetadata() {
    if (!state.selected) return;
    const field = ui.metadataField.value;
    if (!window.confirm(`清除${METADATA_LABELS[field]}的手動值？系統會改用下一順位的資料。`)) return;
    try {
      const collection = await api(`/api/collections/${state.selected.id}/metadata/${field}`, { method: "DELETE" });
      replaceSelected(collection);
      invalidateDerivedData();
      if (ui.metadataEvidence.open) loadMetadataEvidence(true);
      ui.metadataDialog.close();
      toast(`已清除${METADATA_LABELS[field]}的手動值`);
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
    renderDetail(collection);
    if (index >= 0) renderCollections();
  }

  function toggleCollectionSelection(collection, checked) {
    if (checked) {
      state.selectedIds.add(collection.id);
      state.selectedRecords.set(collection.id, collection);
    } else {
      state.selectedIds.delete(collection.id);
      state.selectedRecords.delete(collection.id);
    }
    updateSelectionUI();
  }

  function selectCurrentPage() {
    state.items.forEach((collection) => {
      state.selectedIds.add(collection.id);
      state.selectedRecords.set(collection.id, collection);
    });
    syncResultCheckboxes();
    updateSelectionUI();
  }

  function invertCurrentPageSelection() {
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
    syncResultCheckboxes();
    updateSelectionUI();
  }

  function updateSelectionCheckbox(checkbox, checked = checkbox.checked) {
    checkbox.checked = checked;
    const title = checkbox.dataset.collectionTitle || "這本收藏";
    checkbox.setAttribute("aria-label", checked ? `從批次選取移除 ${title}` : `將 ${title} 加入批次選取`);
  }

  function syncResultCheckboxes() {
    document.querySelectorAll(".collection-checkbox").forEach((checkbox) => {
      const id = Number(checkbox.closest(".collection-item")?.querySelector("[data-collection-id]")?.dataset.collectionId);
      updateSelectionCheckbox(checkbox, state.selectedIds.has(id));
    });
  }

  function updateSelectionUI() {
    if (!ui.selectionRail) return;
    const count = state.selectedIds.size;
    ui.selectionRail.hidden = count === 0;
    ui.selectionCount.textContent = String(count);
    updateLibrarySummary();
    updateWorkbenchBadge();
    renderWorkbenchSelection();
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
      ? `本次操作清單包含 ${collections.length} 筆目前頁面的收藏。`
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
      item.append(cover, copy, remove);
      ui.selectedCollectionList.append(item);
    });
  }

  function syncBatchMetadataField() {
    const isClassification = ui.batchMetadataForm.elements.field.value === "classification";
    const label = byId("batch-metadata-value-label");
    label.firstChild.textContent = isClassification ? "新的種類" : "新的原作";
    ui.batchMetadataForm.elements.value.placeholder = isClassification ? "例如：同人誌、商業誌或其他" : "";
  }

  async function batchAddTag(event) {
    event.preventDefault();
    const name = String(new FormData(ui.batchTagForm).get("tag") || "").trim();
    if (!name) {
      toast("請輸入要加入的標籤", true);
      return;
    }
    const collections = selectedCollections();
    const unchanged = collections.filter((collection) => collection.tags?.includes(name));
    const targets = collections.filter((collection) => !collection.tags?.includes(name));
    const outcomes = await runSelectedRequests(
      (collection) => api(`/api/collections/${collection.id}/tags`, { method: "POST", body: { name } }),
      "正在加入標籤…",
      targets,
    );
    outcomes.unchanged = unchanged;
    outcomes.succeeded.forEach((entry) => state.selectedRecords.set(entry.collection.id, entry.result));
    renderWorkbenchSelection();
    renderClientBatchResult(`批次加入標籤「${name}」`, outcomes);
    invalidateDerivedData();
    ui.batchTagForm.reset();
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
    const outcomes = await runSelectedRequests(
      (collection) => api(`/api/collections/${collection.id}/metadata/${field}`, { method: "PUT", body: { value } }),
      `正在批次寫入${METADATA_LABELS[field]}…`,
    );
    outcomes.succeeded.forEach((entry) => state.selectedRecords.set(entry.collection.id, entry.result));
    renderWorkbenchSelection();
    renderClientBatchResult(`批次寫入${METADATA_LABELS[field]}「${value}」`, outcomes);
    ui.batchMetadataForm.elements.value.value = "";
    invalidateDerivedData();
  }

  async function runSelectedRequests(request, loadingLabel, collections = selectedCollections()) {
    const submitters = document.querySelectorAll("#batch-tools button");
    submitters.forEach((button) => { button.disabled = true; });
    ui.workbenchSelectionSummary.textContent = loadingLabel;
    const succeeded = [];
    const failed = [];
    for (const collection of collections) {
      try {
        const result = await request(collection);
        succeeded.push({ collection, result });
      } catch (error) {
        failed.push({ collection, error });
      }
    }
    submitters.forEach((button) => { button.disabled = false; });
    return { succeeded, failed };
  }

  function renderClientBatchResult(title, outcomes) {
    const unchanged = outcomes.unchanged || [];
    const summary = `更新 ${outcomes.succeeded.length} 筆，未變更 ${unchanged.length} 筆，失敗 ${outcomes.failed.length} 筆`;
    ui.batchResult.hidden = false;
    ui.batchResultSummary.replaceChildren();
    ui.batchResultSummary.append(
      el("strong", "", title),
      el("span", "", summary),
    );
    ui.batchResultItems.replaceChildren();
    outcomes.succeeded.forEach(({ collection }) => ui.batchResultItems.append(batchResultItem(collection, "succeeded", "完成")));
    unchanged.forEach((collection) => ui.batchResultItems.append(batchResultItem(collection, "unchanged", "已具有相同值")));
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
      byId("move-summary").textContent = `準備搬移 ${collections.length} 筆收藏。只有新收藏來源可以搬移；其他項目會逐筆回報失敗。`;
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
    byId("delete-summary").textContent = `準備刪除 ${collections.length} 筆收藏。請先選擇是否需要經過資源回收桶。`;
    renderConfirmItems(byId("delete-item-list"), collections);
    syncDeleteMode();
    ui.deleteDialog.showModal();
  }

  function syncDeleteMode() {
    const permanent = ui.deleteForm.elements.mode.value === "permanent";
    const phrase = `永久刪除 ${state.selectedIds.size} 筆`;
    ui.permanentConfirmPhrase.textContent = phrase;
    ui.permanentConfirmGroup.hidden = !permanent;
    byId("permanent-confirm-note").hidden = !permanent;
    const submit = byId("confirm-delete");
    submit.textContent = permanent ? `永久刪除 ${state.selectedIds.size} 筆` : "移到資源回收桶";
    submit.disabled = permanent && ui.deleteForm.elements.confirmation.value !== phrase;
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
    try {
      const settings = await api("/api/settings");
      const roots = await api("/api/library-roots");
      ui.settingsForm.elements.viewer_path.value = settings.viewer_path;
      ui.settingsForm.elements.thumb_size.value = settings.thumb_size;
      ui.settingsForm.elements.thumb_quality.value = settings.thumb_quality;
      ui.environmentOverrides.textContent = settings.environment_overrides.length
        ? `目前由環境變數覆寫：${settings.environment_overrides.join("、")}；環境變數具有最高優先權。`
        : "目前沒有環境變數覆寫這些設定；環境變數具有最高優先權。";
      renderRoots(roots.roots);
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function saveSettings(event) {
    event.preventDefault();
    const form = new FormData(ui.settingsForm);
    const submit = ui.settingsForm.querySelector('[type="submit"]');
    submit.disabled = true;
    try {
      const settings = await api("/api/settings", {
        method: "PUT",
        body: {
          viewer_path: String(form.get("viewer_path") || "").trim(),
          thumb_size: String(form.get("thumb_size") || "").trim(),
          thumb_quality: Number(form.get("thumb_quality")),
        },
      });
      const requeued = settings.thumbnails_requeued || 0;
      toast(requeued ? `設定已儲存，${formatNumber(requeued)} 張縮圖已排入重建` : "設定已儲存");
      loadSettingsPage();
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
      if (root.active) {
        const deactivate = el("button", "text-button danger-text", "停用");
        deactivate.type = "button";
        deactivate.addEventListener("click", () => deactivateRoot(root));
        item.append(deactivate);
      } else {
        item.append(el("span", "root-action-placeholder", ""));
      }
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
      toast(`已登記資料夾「${root.label}」`);
      loadSettingsPage();
    } catch (error) {
      toast(error.message, true);
    } finally {
      submit.disabled = false;
    }
  }

  async function deactivateRoot(root) {
    if (!window.confirm(`停用資料夾來源「${root.label}」？這不會刪除磁碟檔案或既有收藏紀錄。`)) return;
    try {
      await api(`/api/library-roots/${root.id}`, { method: "DELETE" });
      toast(`已停用「${root.label}」`);
      loadSettingsPage();
    } catch (error) {
      toast(error.message, true);
    }
  }

  async function startScan() {
    if (state.selectedIds.size > 0 && !confirmSelectionClear()) return;
    if (state.selectedIds.size > 0) clearSelection();
    const original = ui.scanButton.textContent;
    ui.scanButton.disabled = true;
    ui.scanButton.textContent = "掃描中…";
    state.activityScan = { status: "running", message: "正在掃描資料夾來源", updatedAt: new Date().toISOString() };
    renderActivityCenter();
    try {
      const report = await api("/api/scans", { method: "POST" });
      const summary = report.summary;
      const prefix = report.status === "partial" ? "掃描部分完成" : "掃描完成";
      state.activityScan = {
        status: report.status === "partial" ? "partial" : "succeeded",
        message: `新增 ${formatNumber(summary.added)}、略過 ${formatNumber(summary.skipped)}、問題 ${formatNumber(report.issues.length)}`,
        updatedAt: new Date().toISOString(),
      };
      toast(`${prefix}：新增 ${formatNumber(summary.added)}、略過 ${formatNumber(summary.skipped)}、問題 ${formatNumber(report.issues.length)}`, report.status === "partial");
      invalidateDerivedData({ library: true });
      state.page = 1;
      state.libraryFocusId = null;
      state.libraryDataKey = null;
    } catch (error) {
      state.activityScan = { status: "failed", message: error.message, updatedAt: new Date().toISOString() };
      toast(error.message, true);
    } finally {
      ui.scanButton.disabled = false;
      ui.scanButton.textContent = original;
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
      setFilterPanelOpen(false, { restoreFocus: true });
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
      syncResultCheckboxes();
      return;
    }
    if (!["j", "k", "J", "K"].includes(event.key)) return;
    event.preventDefault();
    if (!state.items.length) return;
    const current = state.items.findIndex((item) => item.id === state.selected?.id);
    const direction = event.key.toLowerCase() === "j" ? 1 : -1;
    const next = Math.min(state.items.length - 1, Math.max(0, (current < 0 ? 0 : current) + direction));
    selectCollection(state.items[next], true);
  }

  function isDialogOpen() {
    return Boolean(document.querySelector("dialog[open]"));
  }

  function invalidateDerivedData({ library = false } = {}) {
    state.statsLoaded = false;
    state.statsData = null;
    state.shelfLoaded = false;
    state.shelfData = null;
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
