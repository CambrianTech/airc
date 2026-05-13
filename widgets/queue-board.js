/*
 * AIRC queue board widget.
 *
 * Static/read-only by design: GitHub issues remain the source of truth.
 * Embed:
 *   <link rel="stylesheet" href="./queue-board.css">
 *   <script src="./queue-board.js" defer></script>
 *   <airc-queue-board repo="CambrianTech/airc"></airc-queue-board>
 */
(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory();
  } else {
    root.AircQueueBoard = factory();
  }
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  const STATUSES = ["claimed", "in-progress", "blocked", "review", "merged"];
  const CARD_RE = /```json\s*\n([\s\S]*?)\n\s*```/g;

  function parseQueueCard(body) {
    const text = String(body || "");
    CARD_RE.lastIndex = 0;
    let match;
    while ((match = CARD_RE.exec(text)) !== null) {
      try {
        const parsed = JSON.parse(match[1].trim());
        if (parsed && parsed.kind === "airc-queue-card-v1") {
          return parsed;
        }
      } catch (_) {
        // Keep scanning; issues may contain other fenced JSON examples.
      }
    }
    return null;
  }

  function normalizeIssue(issue) {
    const card = parseQueueCard(issue && issue.body);
    if (!card) return null;
    return {
      number: issue.number,
      title: issue.title || "",
      url: issue.html_url || issue.url || "",
      updatedAt: issue.updated_at || issue.updatedAt || "",
      card,
    };
  }

  function groupCards(items) {
    const groups = {};
    for (const status of STATUSES) groups[status] = [];
    groups.other = [];

    for (const item of items) {
      const status = (item.card.status || "other").trim();
      const bucket = Object.prototype.hasOwnProperty.call(groups, status)
        ? status
        : "other";
      groups[bucket].push(item);
    }
    return groups;
  }

  function escapeHtml(value) {
    return String(value == null ? "" : value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function cardHtml(item) {
    const card = item.card;
    const owner = card.owner ? `<span title="Owner">${escapeHtml(card.owner)}</span>` : "";
    const heartbeat = card.last_heartbeat
      ? `<span title="Last heartbeat">${escapeHtml(card.last_heartbeat)}</span>`
      : "";
    const meta = [owner, heartbeat].filter(Boolean).join("");
    const url = escapeHtml(item.url);
    const title = escapeHtml(item.title.replace(/^airc-queue:\s*/i, ""));
    return `
      <article class="airc-q-card">
        <a class="airc-q-card-title" href="${url}" target="_blank" rel="noreferrer">#${escapeHtml(item.number)} ${title}</a>
        <dl class="airc-q-fields">
          ${card.id ? `<div><dt>ID</dt><dd>${escapeHtml(card.id)}</dd></div>` : ""}
          ${card.branch ? `<div><dt>Branch</dt><dd>${escapeHtml(card.branch)}</dd></div>` : ""}
          ${card.env ? `<div><dt>Env</dt><dd>${escapeHtml(card.env)}</dd></div>` : ""}
          ${card.next_action ? `<div><dt>Next</dt><dd>${escapeHtml(card.next_action)}</dd></div>` : ""}
        </dl>
        ${meta ? `<div class="airc-q-meta">${meta}</div>` : ""}
      </article>
    `;
  }

  function renderQueueBoard(container, items, options) {
    const statuses = (options && options.statuses) || STATUSES;
    const groups = groupCards(items);
    const repo = options && options.repo ? options.repo : "";
    const columns = statuses
      .filter((status) => groups[status])
      .map((status) => `
        <section class="airc-q-column" data-status="${escapeHtml(status)}">
          <header>
            <h3>${escapeHtml(status)}</h3>
            <span>${groups[status].length}</span>
          </header>
          <div class="airc-q-list">
            ${groups[status].map(cardHtml).join("") || '<p class="airc-q-empty">Empty</p>'}
          </div>
        </section>
      `)
      .join("");

    container.innerHTML = `
      <section class="airc-q-board" data-repo="${escapeHtml(repo)}">
        <header class="airc-q-header">
          <h2>AIRC Queue</h2>
          ${repo ? `<a href="https://github.com/${escapeHtml(repo)}/issues?q=is%3Aissue%20label%3Aairc-queue" target="_blank" rel="noreferrer">${escapeHtml(repo)}</a>` : ""}
        </header>
        <div class="airc-q-columns">${columns}</div>
      </section>
    `;
  }

  async function fetchQueueIssues(repo, options) {
    if (!repo || !/^[^/]+\/[^/]+$/.test(repo)) {
      throw new Error("AIRC queue widget requires repo=\"owner/repo\"");
    }
    const limit = (options && options.limit) || 50;
    const url = `https://api.github.com/repos/${repo}/issues?state=open&labels=airc-queue&per_page=${encodeURIComponent(limit)}`;
    const response = await fetch(url, {
      headers: {
        Accept: "application/vnd.github+json",
      },
    });
    if (!response.ok) {
      throw new Error(`GitHub issue fetch failed: ${response.status}`);
    }
    const issues = await response.json();
    return issues.map(normalizeIssue).filter(Boolean);
  }

  let AircQueueBoardElement = null;
  if (typeof HTMLElement !== "undefined") {
    AircQueueBoardElement = class extends HTMLElement {
      connectedCallback() {
        this.load();
      }

      async load() {
        const repo = this.getAttribute("repo") || "";
        const limit = Number(this.getAttribute("limit") || "50");
        this.innerHTML = '<div class="airc-q-loading">Loading queue...</div>';
        try {
          const items = await fetchQueueIssues(repo, { limit });
          renderQueueBoard(this, items, { repo });
        } catch (error) {
          this.innerHTML = `<div class="airc-q-error">${escapeHtml(error.message || error)}</div>`;
        }
      }
    };
  }

  if (AircQueueBoardElement && typeof customElements !== "undefined" && !customElements.get("airc-queue-board")) {
    customElements.define("airc-queue-board", AircQueueBoardElement);
  }

  return {
    STATUSES,
    parseQueueCard,
    normalizeIssue,
    groupCards,
    renderQueueBoard,
    fetchQueueIssues,
  };
});
