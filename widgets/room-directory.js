/*
 * AIRC room directory widget.
 *
 * Static/read-only. It renders a public/approved room list supplied as JSON
 * either inline or from a config URL. Do not put private gist ids or secrets in
 * a public config.
 */
(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory();
  } else {
    root.AircRoomDirectory = factory();
  }
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  function normalizeRoom(room) {
    if (!room || !room.name) return null;
    return {
      name: String(room.name),
      scope: room.scope ? String(room.scope) : "",
      description: room.description ? String(room.description) : "",
      visibility: room.visibility ? String(room.visibility) : "approved",
      joinHint: room.joinHint ? String(room.joinHint) : "",
    };
  }

  function normalizeDirectory(config) {
    const rooms = Array.isArray(config && config.rooms) ? config.rooms : [];
    return {
      title: config && config.title ? String(config.title) : "AIRC Rooms",
      rooms: rooms.map(normalizeRoom).filter(Boolean),
    };
  }

  function escapeHtml(value) {
    return String(value == null ? "" : value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function roomHtml(room) {
    return `
      <article class="airc-room">
        <div class="airc-room-main">
          <h3>${escapeHtml(room.name)}</h3>
          ${room.description ? `<p>${escapeHtml(room.description)}</p>` : ""}
        </div>
        <dl>
          ${room.scope ? `<div><dt>Scope</dt><dd>${escapeHtml(room.scope)}</dd></div>` : ""}
          <div><dt>Visibility</dt><dd>${escapeHtml(room.visibility)}</dd></div>
          ${room.joinHint ? `<div><dt>Join</dt><dd><code>${escapeHtml(room.joinHint)}</code></dd></div>` : ""}
        </dl>
      </article>
    `;
  }

  function renderRoomDirectory(container, config) {
    const directory = normalizeDirectory(config);
    container.innerHTML = `
      <section class="airc-room-directory">
        <header>
          <h2>${escapeHtml(directory.title)}</h2>
          <span>${directory.rooms.length}</span>
        </header>
        <div class="airc-room-list">
          ${directory.rooms.map(roomHtml).join("") || '<p class="airc-q-empty">No rooms published.</p>'}
        </div>
      </section>
    `;
  }

  async function loadDirectoryConfig(element) {
    const src = element.getAttribute("src");
    if (src) {
      const response = await fetch(src, { headers: { Accept: "application/json" } });
      if (!response.ok) throw new Error(`Room config fetch failed: ${response.status}`);
      return response.json();
    }
    const script = element.querySelector('script[type="application/json"]');
    if (script) return JSON.parse(script.textContent || "{}");
    return { rooms: [] };
  }

  let AircRoomDirectoryElement = null;
  if (typeof HTMLElement !== "undefined") {
    AircRoomDirectoryElement = class extends HTMLElement {
      connectedCallback() {
        this.load();
      }

      async load() {
        this.innerHTML = '<div class="airc-q-loading">Loading rooms...</div>';
        try {
          const config = await loadDirectoryConfig(this);
          renderRoomDirectory(this, config);
        } catch (error) {
          this.innerHTML = `<div class="airc-q-error">${escapeHtml(error.message || error)}</div>`;
        }
      }
    };
  }

  if (AircRoomDirectoryElement && typeof customElements !== "undefined" && !customElements.get("airc-room-directory")) {
    customElements.define("airc-room-directory", AircRoomDirectoryElement);
  }

  return {
    normalizeRoom,
    normalizeDirectory,
    renderRoomDirectory,
    loadDirectoryConfig,
  };
});
