// NETIX Republisher web GUI.
//
// Feature-parity port of the capability-driven iced desktop app: every control
// is rendered from the protocol's declared capabilities (FieldSpec), so adding
// a protocol never requires touching this file. State hydrates from the SSE
// snapshot and stays live via deltas; REST calls mirror desktop actions 1:1.

"use strict";

// ---------- tiny DOM helpers (CSP: no innerHTML with data, no inline style) ----------

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === "class") node.className = value;
    else if (key === "dataset") Object.assign(node.dataset, value);
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (value !== undefined && value !== null) node.setAttribute(key, value);
  }
  for (const child of children) {
    if (child == null) continue;
    node.append(child.nodeType ? child : document.createTextNode(String(child)));
  }
  return node;
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

const $ = (id) => document.getElementById(id);

// ---------- state ----------

const state = {
  caps: [],            // CapabilitiesDto[]
  config: null,        // redacted AppConfig
  secrets: { password_set: false, client_key_passphrase_set: false },
  status: null,        // status_json
  devices: [],
  browsed: [],
  points: [],          // PointDto[]
  samples: [],         // SampleDto[], newest first
  logs: [],            // LogRecord[]
  statuses: {},        // identity -> PointStatusDto
  lifecycle: { state: "stopped", error: null },
  interfaces: [],
  connValues: {},      // key -> string, mirrors desktop conn_values
  pe: { editing: null, addr: {} },
  clearArmed: false,
  page: "overview",
};

const activeCaps = () =>
  state.caps.find((c) => c.id === (state.config ? state.config.protocol : "")) || null;

function jsonScalar(value) {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  return JSON.stringify(value);
}

const defaultFieldString = (spec) =>
  spec.default !== undefined && spec.default !== null ? jsonScalar(spec.default) : "";

/// Mirrors desktop connection_strings(): stored value or spec default, as strings.
function connectionStrings() {
  const caps = activeCaps();
  const values = {};
  if (!caps || !state.config) return values;
  const existing = state.config.connections?.[state.config.protocol] || {};
  for (const spec of caps.connection_fields) {
    values[spec.key] =
      existing[spec.key] !== undefined ? jsonScalar(existing[spec.key]) : defaultFieldString(spec);
  }
  return values;
}

// ---------- API ----------

async function api(path, options = {}) {
  const response = await fetch(path, {
    credentials: "same-origin",
    headers: options.body ? { "Content-Type": "application/json" } : {},
    ...options,
  });
  if (response.status === 401) {
    showLogin();
    throw new Error("authentication required");
  }
  let body = null;
  try {
    body = await response.json();
  } catch {
    /* non-JSON response */
  }
  if (!response.ok) {
    const message = body && body.error ? body.error : `${response.status} ${response.statusText}`;
    toast(message);
    throw new Error(message);
  }
  return body;
}

let toastTimer = null;
function toast(message, kind = "error") {
  const node = $("toast");
  node.textContent = message;
  node.className = kind === "info" ? "toast info" : "toast";
  node.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    node.hidden = true;
  }, 5000);
}

// ---------- login ----------

function showLogin() {
  $("shell").hidden = true;
  $("login").hidden = false;
  $("login-password").focus();
}

function showShell(authRequired) {
  $("login").hidden = true;
  $("shell").hidden = false;
  $("logout-btn").hidden = !authRequired;
}

async function initSession() {
  const session = await api("/api/session");
  if (session.auth_required && !session.authenticated) {
    showLogin();
  } else {
    showShell(session.auth_required);
    connectEvents();
  }
  $("login-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const password = $("login-password").value;
    try {
      await api("/api/session", { method: "POST", body: JSON.stringify({ password }) });
      $("login-error").hidden = true;
      $("login-password").value = "";
      showShell(true);
      connectEvents();
    } catch (error) {
      const node = $("login-error");
      node.textContent = error.message;
      node.hidden = false;
    }
  });
  $("logout-btn").addEventListener("click", async () => {
    try {
      await api("/api/session", { method: "DELETE" });
    } finally {
      if (events) events.close();
      showLogin();
    }
  });
}

// ---------- SSE ----------

let events = null;

function setConnIndicator(live) {
  const node = $("conn-indicator");
  clear(node);
  node.append(el("span", { class: "dot" }), live ? "Live" : "Reconnecting");
  node.classList.toggle("offline", !live);
}

function connectEvents() {
  if (events) events.close();
  events = new EventSource("/api/events");
  events.onopen = () => setConnIndicator(true);
  events.onerror = () => setConnIndicator(false);
  events.onmessage = (message) => {
    let payload;
    try {
      payload = JSON.parse(message.data);
    } catch {
      return;
    }
    handleEvent(payload);
  };
}

function handleEvent(ev) {
  switch (ev.type) {
    case "snapshot": {
      state.status = ev.status;
      state.caps = ev.capabilities;
      state.config = ev.config.config;
      state.secrets = ev.config.secrets;
      state.devices = ev.devices;
      state.browsed = ev.browsed;
      state.points = ev.points;
      state.samples = ev.recent_samples;
      state.logs = ev.logs;
      state.lifecycle = ev.status.lifecycle;
      state.statuses = {};
      for (const point of state.points) state.statuses[point.identity] = point.status;
      state.connValues = connectionStrings();
      applyTheme();
      resetPointEditor();
      renderAll();
      loadInterfaces();
      break;
    }
    case "log":
      state.logs.push(ev.record);
      if (state.logs.length > 500) state.logs.shift();
      if (ev.status_line && state.status) state.status.status_line = ev.status_line;
      renderLogs();
      renderStatusBar();
      break;
    case "devices":
      state.devices = ev.devices;
      if (state.status) state.status.devices = ev.devices.length;
      renderConnectLists();
      renderOverview();
      renderStatusBar();
      break;
    case "browsed":
      state.browsed = ev.points;
      renderConnectLists();
      break;
    case "scan_progress":
      state.status && (state.status.scan_progress = { current: ev.current, total: ev.total });
      renderScanProgress(ev.current, ev.total);
      break;
    case "points":
      state.points = ev.points;
      for (const point of state.points) state.statuses[point.identity] = point.status;
      if (state.status) state.status.points = ev.points.length;
      renderPoints();
      renderOverview();
      renderStatusBar();
      break;
    case "samples":
      state.samples = ev.samples.concat(state.samples).slice(0, 200);
      Object.assign(state.statuses, ev.statuses || {});
      syncPointStatuses();
      renderSamples();
      renderOverview();
      renderPoints();
      break;
    case "statuses":
      Object.assign(state.statuses, ev.statuses || {});
      syncPointStatuses();
      renderPoints();
      renderOverview();
      break;
    case "stats":
      if (state.status) {
        state.status.published_total = ev.published_total;
        if (ev.acked_total !== undefined) state.status.acked_total = ev.acked_total;
        // `last_error` is always present in the delta (may be null once the link
        // recovers), so mirror it verbatim to keep the connection banner honest.
        state.status.last_error = ev.last_error ?? null;
      }
      renderRepublish();
      renderOverview();
      break;
    case "lifecycle":
      state.lifecycle = ev.lifecycle;
      if (state.status) state.status.lifecycle = ev.lifecycle;
      renderRepublish();
      break;
    case "config":
      state.config = ev.config.config;
      state.secrets = ev.config.secrets;
      if (state.status) state.status.protocol = state.config.protocol;
      state.connValues = connectionStrings();
      applyTheme();
      renderConnect();
      renderSettings();
      renderRepublish();
      break;
    default:
      break;
  }
}

function syncPointStatuses() {
  for (const point of state.points) {
    if (state.statuses[point.identity]) point.status = state.statuses[point.identity];
  }
}

function applyTheme() {
  const theme = state.config?.ui?.theme || "auto";
  if (theme === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

// ---------- navigation ----------

const PAGES = ["overview", "connect", "points", "republish", "settings", "logs"];

function navigate(page) {
  if (!PAGES.includes(page)) page = "overview";
  state.page = page;
  state.clearArmed = false;
  closeMobileMenu();
  location.hash = page;
  for (const name of PAGES) $(`page-${name}`).hidden = name !== page;
  document.querySelectorAll(".nav-btn[data-page]").forEach((button) => {
    button.classList.toggle("active", button.dataset.page === page);
  });
  renderAll();
}

// ---------- shared render helpers ----------

function chip(label, kind) {
  return el("span", { class: `chip ${kind}` }, label);
}

function metric(label, value, sub, kind) {
  return el(
    "div",
    { class: "metric" },
    el("div", { class: "metric-label" }, label),
    el("div", { class: "metric-value" }, value),
    el("div", { class: "metric-sub" }, chip(sub, kind))
  );
}

// Colour the "Delivered" (broker-acked) metric: green once the broker confirms
// deliveries, but amber when samples have been queued yet nothing is acked — the
// "looks healthy, delivers nothing" case (bad auth / broker rejecting publishes).
function deliveredKind(status) {
  const queued = status.published_total || 0;
  const acked = status.acked_total || 0;
  if (acked > 0 || queued === 0) return "success";
  return "warning";
}

// Honest connection state banner: the "Queued" counter climbs even when the
// broker rejects auth, so on its own the box looks healthy while delivering
// nothing. Show a loud banner whenever the worker reported a connection error
// (carries the CONNACK refusal text on bad auth) or when samples are piling up
// with zero broker acks. Returns an element to prepend into a .metrics grid, or
// null when the link is healthy / idle.
function connectionBanner(status) {
  if (!status) return null;
  const queued = status.published_total || 0;
  const acked = status.acked_total || 0;
  const error = status.last_error || null;
  const notDelivering = queued > 0 && acked === 0;
  if (!error && !notDelivering) return null;
  const title = error
    ? "Broker connection error"
    : "Not delivering to broker";
  const detail = error
    ? error
    : "Samples are being queued locally but the broker has not confirmed any deliveries (check credentials, TLS and broker permissions).";
  return el(
    "div",
    { class: "conn-banner danger" },
    el("span", { class: "conn-banner-icon" }, "⚠"),
    el(
      "div",
      {},
      el("div", { class: "conn-banner-title" }, title),
      el("div", { class: "conn-banner-detail" }, detail)
    )
  );
}

function statusChip(status) {
  const map = {
    unknown: ["Unknown", ""],
    publish_error: ["Publish error", "danger"],
    read_error: ["Read error", "danger"],
    stale: ["Stale", "warning"],
    ok: ["OK", "success"],
  };
  const [label, kind] = map[status?.state] || map.unknown;
  return chip(label, kind);
}

function formatTimestamp(ms) {
  if (!ms) return "—";
  const date = new Date(ms);
  const pad = (n) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

// ---------- overview ----------

function renderOverview() {
  if (state.page !== "overview" || !state.status) return;
  const stale = Object.values(state.statuses).filter((s) => s.stale).length;
  const metrics = $("overview-metrics");
  clear(metrics);
  metrics.append(
    metric("Points", String(state.points.length), "configured", "accent"),
    metric("Devices", String(state.devices.length), "discovered", ""),
    metric("Queued", String(state.status.published_total), "enqueued", ""),
    metric(
      "Delivered",
      String(state.status.acked_total ?? 0),
      "broker-acked",
      deliveredKind(state.status)
    ),
    metric("Stale", String(stale), "points", stale > 0 ? "warning" : "success")
  );
  const banner = connectionBanner(state.status);
  if (banner) metrics.append(banner);
  const activity = $("overview-activity");
  clear(activity);
  if (state.samples.length === 0) {
    activity.append(el("div", { class: "muted" }, "No samples yet."));
  }
  for (const sample of state.samples.slice(0, 8)) {
    activity.append(
      el(
        "div",
        { class: "activity-row" },
        el("div", { class: "activity-name" }, sample.display_name),
        el("div", { class: "activity-value" }, sample.value_display)
      )
    );
  }
}

// ---------- connect ----------

let connSaveTimer = null;

function persistConnection() {
  clearTimeout(connSaveTimer);
  connSaveTimer = setTimeout(() => {
    api("/api/connection", {
      method: "PUT",
      body: JSON.stringify({ values: state.connValues }),
    }).catch(() => {});
  }, 500);
}

function fieldControl(spec, values, onChange) {
  const current = values[spec.key] ?? "";
  if (spec.kind === "bool") {
    const input = el("input", { type: "checkbox" });
    input.checked = current === "true";
    input.addEventListener("change", () => onChange(spec.key, String(input.checked)));
    return el("label", { class: "check-row" }, input, ` ${spec.label}`);
  }
  if (spec.kind === "enum") {
    const select = el("select", { id: `field-${spec.key}` });
    for (const option of spec.options || []) {
      const node = el("option", { value: option }, option);
      if (option === current) node.selected = true;
      select.append(node);
    }
    select.addEventListener("change", () => onChange(spec.key, select.value));
    return el("div", { class: "field-row" }, el("label", { for: `field-${spec.key}` }, spec.label), select);
  }
  const input = el("input", {
    id: `field-${spec.key}`,
    type: spec.kind === "secret" ? "password" : "text",
    value: current,
    placeholder: spec.help || "",
  });
  input.addEventListener("input", () => onChange(spec.key, input.value));
  return el("div", { class: "field-row" }, el("label", { for: `field-${spec.key}` }, spec.label), input);
}

function renderConnect() {
  if (state.page !== "connect" || !state.config) return;
  const container = $("conn-fields");
  // Don't clobber the field the user is typing in.
  if (container.contains(document.activeElement)) return;

  const select = $("protocol-select");
  clear(select);
  for (const caps of state.caps) {
    const option = el("option", { value: caps.id }, caps.display_name);
    if (caps.id === state.config.protocol) option.selected = true;
    select.append(option);
  }

  clear(container);
  const caps = activeCaps();
  if (!caps) {
    container.append(el("div", { class: "muted" }, "No protocols are registered."));
    return;
  }

  const discoverAll = state.connValues["discover_all_interfaces"] === "true";
  const hasInterface = caps.connection_fields.some((spec) => spec.key === "interface");
  for (const spec of caps.connection_fields) {
    if (spec.key === "interface") continue;
    container.append(fieldControl(spec, state.connValues, onConnChange));
  }
  if (hasInterface) {
    if (!discoverAll) {
      const select = el("select", { id: "field-interface" });
      const current = state.connValues["interface"] || "";
      const known = state.interfaces.some((iface) => iface.addr === current);
      if (current && !known) select.append(el("option", { value: current, selected: "" }, current));
      for (const iface of state.interfaces) {
        const option = el("option", { value: iface.addr }, `${iface.name} (${iface.addr})`);
        if (iface.addr === current) option.selected = true;
        select.append(option);
      }
      select.addEventListener("change", () => onConnChange("interface", select.value));
      const refresh = el("button", { class: "btn", type: "button" }, "Refresh NICs");
      refresh.addEventListener("click", loadInterfaces);
      container.append(
        el(
          "div",
          { class: "field-row" },
          el("label", { for: "field-interface" }, "Network interface"),
          select,
          refresh
        )
      );
    } else {
      const spec = caps.connection_fields.find((s) => s.key === "interface");
      container.append(fieldControl(spec, state.connValues, onConnChange));
    }
  }

  $("discover-btn").textContent = caps.discovery_label;
  renderConnectLists();
}

function onConnChange(key, value) {
  state.connValues[key] = value;
  persistConnection();
  if (key === "discover_all_interfaces") renderConnect();
}

async function loadInterfaces() {
  try {
    const payload = await api("/api/interfaces");
    state.interfaces = payload.interfaces;
    if (!state.connValues["interface"] && state.interfaces.length > 0) {
      // Preselect locally only — persisting here would write config to disk
      // merely from viewing the page. It is saved when the user edits anything.
      state.connValues["interface"] = state.interfaces[0].addr;
    }
    renderConnect();
  } catch {
    /* toast already shown */
  }
}

function renderScanProgress(current, total) {
  const node = $("scan-progress");
  node.hidden = false;
  node.textContent = `Scan progress: ${current}/${total}`;
}

function renderConnectLists() {
  if (state.page !== "connect") return;
  $("scanall-btn").hidden = state.devices.length === 0;

  const devicesBlock = $("devices-block");
  const devicesList = $("devices-list");
  devicesBlock.hidden = state.devices.length === 0;
  clear(devicesList);
  for (const device of state.devices) {
    const browse = el("button", { class: "btn" }, "Browse");
    browse.addEventListener("click", () =>
      api(`/api/devices/${device.index}/browse`, { method: "POST" }).catch(() => {})
    );
    devicesList.append(
      el(
        "div",
        { class: "row-card" },
        el(
          "div",
          { class: "row-main" },
          el("div", { class: "row-title" }, device.key),
          el("div", { class: "row-sub" }, `${device.address} — ${device.detail}`)
        ),
        browse
      )
    );
  }

  const browsedBlock = $("browsed-block");
  const browsedList = $("browsed-list");
  browsedBlock.hidden = state.browsed.length === 0;
  clear(browsedList);
  for (const point of state.browsed) {
    const add = el("button", { class: "btn" }, "Add");
    add.addEventListener("click", () =>
      api(`/api/browsed/${point.index}/add`, { method: "POST" }).catch(() => {})
    );
    const name = point.name || point.suggested_tag_path;
    const value = point.value_display ?? "—";
    browsedList.append(
      el(
        "div",
        { class: "row-card" },
        el(
          "div",
          { class: "row-main" },
          el("div", { class: "row-title" }, name),
          el("div", { class: "row-sub" }, `${point.addressing_display}  =  ${value}`)
        ),
        add
      )
    );
  }
}

// ---------- points ----------

function resetPointEditor() {
  state.pe.editing = null;
  state.pe.addr = {};
  const caps = activeCaps();
  for (const spec of caps?.addressing_fields || []) {
    state.pe.addr[spec.key] = defaultFieldString(spec);
  }
  $("pe-device-key").value = "";
  $("pe-tag-path").value = "";
  $("pe-poll").value = "10";
  $("pe-enabled").checked = true;
  $("point-editor-title").textContent = "New point";
  renderPointEditorFields();
}

function loadPointIntoEditor(point) {
  state.pe.editing = point.index;
  state.pe.addr = {};
  const caps = activeCaps();
  for (const spec of caps?.addressing_fields || []) {
    state.pe.addr[spec.key] =
      point.addressing[spec.key] !== undefined
        ? jsonScalar(point.addressing[spec.key])
        : defaultFieldString(spec);
  }
  $("pe-device-key").value = point.device_key;
  $("pe-tag-path").value = point.tag_path;
  $("pe-poll").value = String(point.poll_interval_secs);
  $("pe-enabled").checked = point.enabled;
  $("point-editor-title").textContent = "Edit point";
  renderPointEditorFields();
}

function renderPointEditorFields() {
  const container = $("pe-addressing");
  clear(container);
  const caps = activeCaps();
  for (const spec of caps?.addressing_fields || []) {
    container.append(
      fieldControl(spec, state.pe.addr, (key, value) => {
        state.pe.addr[key] = value;
      })
    );
  }
}

async function savePoint() {
  const body = {
    index: state.pe.editing,
    enabled: $("pe-enabled").checked,
    device_key: $("pe-device-key").value,
    tag_path: $("pe-tag-path").value,
    poll_interval_secs: Math.max(1, parseInt($("pe-poll").value, 10) || 10),
    addressing: state.pe.addr,
  };
  try {
    await api("/api/points", { method: "POST", body: JSON.stringify(body) });
    resetPointEditor();
  } catch {
    /* toast shown */
  }
}

function renderPoints() {
  if (state.page !== "points") return;
  $("points-title").textContent = `Configured points (${state.points.length})`;
  const clearButton = $("clear-points");
  clearButton.textContent = state.clearArmed ? "Confirm clear" : "Clear all points";
  clearButton.classList.toggle("danger", state.clearArmed);

  const list = $("points-list");
  clear(list);
  for (const point of state.points) {
    const toggle = el("input", { type: "checkbox" });
    toggle.checked = point.enabled;
    toggle.addEventListener("change", () =>
      api(`/api/points/${point.index}`, {
        method: "PATCH",
        body: JSON.stringify({ enabled: toggle.checked }),
      }).catch(() => {})
    );
    const edit = el("button", { class: "btn ghost" }, "Edit");
    edit.addEventListener("click", () => loadPointIntoEditor(point));
    const remove = el("button", { class: "btn ghost-danger" }, "Delete");
    remove.addEventListener("click", () =>
      api(`/api/points/${point.index}`, { method: "DELETE" })
        .then(resetPointEditor)
        .catch(() => {})
    );
    const detail = point.status.last_value_display ?? "—";
    const sampled = point.status.last_sample_ms ? formatTimestamp(point.status.last_sample_ms) : "—";
    list.append(
      el(
        "div",
        { class: "row-card" },
        toggle,
        el(
          "div",
          { class: "row-main" },
          el("div", { class: "row-title", title: point.display_name }, point.display_name),
          el("div", { class: "row-sub" }, `${point.topic}  ·  ${detail}  ·  sampled ${sampled}`)
        ),
        statusChip(point.status),
        edit,
        remove
      )
    );
  }
}

// ---------- republish ----------

function renderRepublish() {
  if (state.page !== "republish" || !state.config || !state.status) return;
  const lifecycle = state.lifecycle || { state: "stopped" };
  const running = ["starting", "running", "stopping"].includes(lifecycle.state);
  const toggle = $("run-toggle");
  toggle.textContent = running ? "Stop" : "Start";
  toggle.className = running ? "btn danger" : "btn primary";

  const readout = $("mqtt-readout");
  clear(readout);
  const mqtt = state.config.mqtt;
  const endpoint = `${mqtt.host}:${mqtt.port} (${mqtt.use_tls ? "TLS" : "plain TCP"})`;
  for (const [label, value] of [
    ["Endpoint", endpoint],
    ["Topic prefix", mqtt.topic_prefix],
    ["Health topic", mqtt.health_topic],
  ]) {
    readout.append(
      el(
        "div",
        { class: "readout" },
        el("div", { class: "readout-label" }, label),
        el("div", { class: "readout-value" }, value)
      )
    );
  }

  const lifecycleChips = {
    running: ["Running", "success"],
    starting: ["Starting", "warning"],
    stopping: ["Stopping", "warning"],
    stopped: ["Stopped", ""],
    failed: ["Failed", "danger"],
  };
  const [label, kind] = lifecycleChips[lifecycle.state] || lifecycleChips.stopped;
  const metrics = $("republish-metrics");
  clear(metrics);
  metrics.append(
    metric("State", label, state.config.protocol || "—", kind),
    metric("Queued", String(state.status.published_total), "enqueued", ""),
    metric(
      "Delivered",
      String(state.status.acked_total ?? 0),
      "broker-acked",
      deliveredKind(state.status)
    ),
    metric("Points", String(state.points.length), "configured", "")
  );
  const banner = connectionBanner(state.status);
  if (banner) metrics.append(banner);
  renderSamples();
}

function renderSamples() {
  if (state.page !== "republish") return;
  const list = $("samples-list");
  clear(list);
  for (const sample of state.samples.slice(0, 40)) {
    list.append(
      el(
        "div",
        { class: "sample-row" },
        el("div", { class: "sample-topic" }, sample.topic),
        el("div", { class: "sample-value" }, sample.value_display)
      )
    );
  }
}

// ---------- settings ----------

function settingsRow(label, input, help) {
  const row = el("div", { class: "field-row" }, el("label", { for: input.id }, label), input);
  if (help) row.append(el("span", { class: "field-help" }, help));
  return row;
}

function textInput(id, value, placeholder = "") {
  return el("input", { id, value: value ?? "", placeholder });
}

function renderSettings() {
  if (state.page !== "settings" || !state.config) return;
  const container = $("settings-fields");
  if (container.contains(document.activeElement)) return;
  clear(container);
  const mqtt = state.config.mqtt;

  const host = textInput("set-host", mqtt.host);
  const port = textInput("set-port", String(mqtt.port));
  const tls = el("input", { id: "set-tls", type: "checkbox" });
  tls.checked = mqtt.use_tls;
  const clientId = textInput("set-client-id", mqtt.client_id);
  const topicPrefix = textInput("set-topic-prefix", mqtt.topic_prefix);
  const payload = el("select", { id: "set-payload" });
  for (const [value, label] of [
    ["scalar", "Scalar (value per topic)"],
    ["netix_envelope", "Netix envelope (per device)"],
  ]) {
    const option = el("option", { value }, label);
    if (mqtt.payload_format === value) option.selected = true;
    payload.append(option);
  }
  const deviceTopicPrefix = textInput("set-device-topic", mqtt.device_topic_prefix, "envelope mode, e.g. /Netix/Sim/Device");
  const healthTopic = textInput("set-health-topic", mqtt.health_topic);
  const username = textInput("set-username", mqtt.username ?? "", "optional");
  const password = el("input", {
    id: "set-password",
    type: "password",
    placeholder: state.secrets.password_set ? "(set — leave blank to keep)" : "optional",
  });
  password.addEventListener("input", () => (password.dataset.dirty = "1"));
  const clearPassword = el("button", { class: "btn", type: "button" }, "Clear");
  clearPassword.addEventListener("click", () => {
    password.value = "";
    password.dataset.dirty = "1";
    password.placeholder = "(will be cleared)";
  });
  const caCert = textInput("set-ca-cert", mqtt.ca_cert_path ?? "", "optional");
  const clientCert = textInput("set-client-cert", mqtt.client_cert_path ?? "", "optional");
  const clientKey = textInput("set-client-key", mqtt.client_key_path ?? "", "optional");
  const passphrase = el("input", {
    id: "set-passphrase",
    type: "password",
    placeholder: state.secrets.client_key_passphrase_set ? "(set — leave blank to keep)" : "optional",
  });
  passphrase.addEventListener("input", () => (passphrase.dataset.dirty = "1"));
  const keepAlive = textInput("set-keep-alive", String(mqtt.keep_alive_secs));
  const retain = el("input", { id: "set-retain", type: "checkbox" });
  retain.checked = mqtt.retain;
  const remember = el("input", { id: "set-remember", type: "checkbox" });
  remember.checked = mqtt.remember_secrets;
  const autostart = el("input", { id: "set-autostart", type: "checkbox" });
  autostart.checked = mqtt.autostart;
  // Top-level runtime flag (sibling to autostart), not an mqtt field.
  const discoverOnStart = el("input", { id: "set-discover-on-start", type: "checkbox" });
  discoverOnStart.checked = !!state.config.discover_on_start;
  const theme = el("select", { id: "set-theme" });
  for (const value of ["auto", "light", "dark"]) {
    const option = el("option", { value }, value[0].toUpperCase() + value.slice(1));
    if ((state.config.ui?.theme || "auto") === value) option.selected = true;
    theme.append(option);
  }

  const section = (title) => el("div", { class: "form-section" }, title);
  container.append(
    section("Broker"),
    settingsRow("MQTT host", host),
    settingsRow("MQTT port", port),
    el("label", { class: "check-row" }, tls, " Use TLS"),
    settingsRow("Client ID", clientId),
    settingsRow("Keep-alive (s)", keepAlive),
    section("Topics & payload"),
    settingsRow("Topic prefix", topicPrefix),
    settingsRow("Payload format", payload),
    settingsRow("Device topic prefix", deviceTopicPrefix),
    settingsRow("Health topic", healthTopic),
    el("label", { class: "check-row" }, retain, " Retain"),
    section("Authentication"),
    settingsRow("Username", username),
    el("div", { class: "field-row" }, el("label", { for: "set-password" }, "Password"), password, clearPassword),
    el("label", { class: "check-row" }, remember, " Remember secrets in config"),
    section("TLS certificates"),
    settingsRow("CA cert path", caCert),
    settingsRow("Client cert path", clientCert),
    settingsRow("Client key path", clientKey),
    settingsRow("Client key passphrase", passphrase),
    section("Behavior"),
    el("label", { class: "check-row" }, autostart, " Auto-start republishing on launch"),
    el(
      "label",
      { class: "check-row" },
      discoverOnStart,
      " Discover devices on start when no points are enabled"
    ),
    section("Appearance"),
    settingsRow("Theme", theme)
  );
}

async function saveSettings() {
  const dirty = (id) => $(id) && $(id).dataset.dirty === "1";
  const body = {
    mqtt: {
      host: $("set-host").value,
      port: parseInt($("set-port").value, 10) || 0,
      use_tls: $("set-tls").checked,
      client_id: $("set-client-id").value,
      topic_prefix: $("set-topic-prefix").value,
      health_topic: $("set-health-topic").value,
      username: $("set-username").value,
      password: dirty("set-password") ? $("set-password").value : null,
      ca_cert_path: $("set-ca-cert").value,
      client_cert_path: $("set-client-cert").value,
      client_key_path: $("set-client-key").value,
      client_key_passphrase: dirty("set-passphrase") ? $("set-passphrase").value : null,
      remember_secrets: $("set-remember").checked,
      retain: $("set-retain").checked,
      keep_alive_secs: Math.max(1, parseInt($("set-keep-alive").value, 10) || 30),
      payload_format: $("set-payload").value,
      device_topic_prefix: $("set-device-topic").value,
      autostart: $("set-autostart").checked,
    },
    discover_on_start: $("set-discover-on-start").checked,
    ui: { theme: $("set-theme").value },
  };
  try {
    await api("/api/settings", { method: "PUT", body: JSON.stringify(body) });
    toast("Configuration saved", "info");
  } catch {
    /* toast shown */
  }
}

// ---------- logs ----------

function renderLogs() {
  if (state.page !== "logs") return;
  const list = $("logs-list");
  clear(list);
  const entries = state.logs.slice(-200).reverse();
  for (const entry of entries) {
    list.append(
      el(
        "div",
        { class: `log-row log-${entry.level}` },
        el("span", { class: "log-chip" }, entry.level.toUpperCase()),
        el("span", { class: "log-msg" }, entry.message)
      )
    );
  }
}

// ---------- status bar ----------

function renderStatusBar() {
  if (!state.status) return;
  const bar = $("status-bar");
  clear(bar);
  bar.append(
    el("span", { class: "status-line" }, state.status.status_line),
    el(
      "span",
      { class: "status-meta" },
      `${state.points.length} point${state.points.length === 1 ? "" : "s"} · ` +
        `${state.devices.length} device${state.devices.length === 1 ? "" : "s"}`
    ),
    el("span", { class: "status-path" }, state.status.config_path)
  );
}

function renderAll() {
  renderOverview();
  renderConnect();
  renderPoints();
  renderRepublish();
  renderSettings();
  renderLogs();
  renderStatusBar();
}

// ---------- wire up static controls ----------

function closeMobileMenu() {
  const sidebar = $("sidebar");
  const backdrop = $("backdrop");
  if (sidebar) sidebar.classList.remove("open");
  if (backdrop) backdrop.hidden = true;
}

function initControls() {
  document.querySelectorAll(".nav-btn[data-page]").forEach((button) => {
    button.addEventListener("click", () => navigate(button.dataset.page));
  });

  $("menu-btn").addEventListener("click", () => {
    $("sidebar").classList.add("open");
    $("backdrop").hidden = false;
  });
  $("backdrop").addEventListener("click", closeMobileMenu);

  $("protocol-select").addEventListener("change", async (event) => {
    // Switching clears discovery results and repoints the whole app; make it a
    // deliberate act rather than a silent side effect of exploring the list.
    const caps = state.caps.find((c) => c.id === event.target.value);
    const label = caps ? caps.display_name : event.target.value;
    if (state.config && event.target.value !== state.config.protocol) {
      if (!window.confirm(`Switch the active protocol to ${label}? Discovered devices and browsed points will be cleared.`)) {
        renderConnect();
        return;
      }
    }
    try {
      await api("/api/protocol", { method: "POST", body: JSON.stringify({ id: event.target.value }) });
      state.devices = [];
      state.browsed = [];
      resetPointEditor();
    } catch {
      /* toast shown */
    }
  });

  $("discover-btn").addEventListener("click", () => api("/api/discover", { method: "POST" }).catch(() => {}));
  $("scanall-btn").addEventListener("click", () => api("/api/scan-all", { method: "POST" }).catch(() => {}));
  $("poll-once").addEventListener("click", () => api("/api/poll-once", { method: "POST" }).catch(() => {}));

  $("run-toggle").addEventListener("click", () => {
    const running = ["starting", "running", "stopping"].includes(state.lifecycle?.state);
    const path = running ? "/api/republisher/stop" : "/api/republisher/start";
    api(path, { method: "POST" }).catch(() => {});
  });

  $("pe-save").addEventListener("click", savePoint);
  $("pe-new").addEventListener("click", resetPointEditor);

  $("clear-points").addEventListener("click", () => {
    if (!state.clearArmed) {
      state.clearArmed = true;
      renderPoints();
      setTimeout(() => {
        state.clearArmed = false;
        renderPoints();
      }, 4000);
      return;
    }
    state.clearArmed = false;
    api("/api/points", { method: "DELETE" })
      .then(resetPointEditor)
      .catch(() => {});
  });

  $("settings-save").addEventListener("click", saveSettings);
}

// ---------- boot ----------

window.addEventListener("hashchange", () => navigate(location.hash.slice(1)));

initControls();
navigate(location.hash.slice(1) || "overview");
initSession().catch((error) => toast(error.message));
