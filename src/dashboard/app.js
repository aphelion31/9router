(function () {
  "use strict";

  function showBanner(msg, kind) {
    var el = document.getElementById("error-banner");
    if (!el) {
      el = document.createElement("div");
      el.id = "error-banner";
      el.setAttribute("role", "alert");
      var main = document.querySelector("main") || document.body;
      main.insertBefore(el, main.firstChild);
    }
    el.className = "banner " + (kind === "success" ? "banner-success" : "banner-error");
    el.textContent = String(msg || "Unexpected error");
    el.classList.remove("hidden");
  }

  function clearBanner() {
    var el = document.getElementById("error-banner");
    if (el) el.classList.add("hidden");
  }

  window.onerror = function (message, source, lineno, colno, error) {
    var detail = (error && error.message) || message || "Script error";
    showBanner("Page error: " + detail, "error");
    return false;
  };

  window.onunhandledrejection = function (event) {
    var reason = event && event.reason;
    var detail = (reason && reason.message) || String(reason || "Unhandled error");
    showBanner("Request error: " + detail, "error");
  };

  function escapeHtml(value) {
    return String(value == null ? "" : value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function copyText(text, btn) {
    var str = String(text == null ? "" : text);
    function markCopied() {
      if (!btn) return;
      var orig = btn.getAttribute("data-orig-label") || btn.textContent;
      btn.setAttribute("data-orig-label", orig);
      btn.textContent = "Copied";
      setTimeout(function () {
        btn.textContent = orig;
      }, 1500);
    }
    function fallbackCopy() {
      var ta = document.createElement("textarea");
      ta.value = str;
      ta.setAttribute("readonly", "");
      ta.style.position = "fixed";
      ta.style.left = "-9999px";
      ta.style.top = "0";
      document.body.appendChild(ta);
      ta.select();
      ta.setSelectionRange(0, str.length);
      var ok = false;
      try {
        ok = document.execCommand("copy");
      } catch (e) {
        ok = false;
      }
      document.body.removeChild(ta);
      if (ok) {
        markCopied();
      } else {
        showBanner("Clipboard unavailable; please copy manually.", "error");
      }
    }
    if (navigator.clipboard && typeof navigator.clipboard.writeText === "function") {
      navigator.clipboard.writeText(str).then(markCopied, fallbackCopy);
    } else {
      fallbackCopy();
    }
  }

  async function apiFetch(url, options) {
    var res = await fetch(url, options || {});
    if (res.status === 401) {
      window.location.assign("/login");
      throw new Error("Session expired");
    }
    var text = await res.text();
    var data = null;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch (e) {
        data = { raw: text };
      }
    }
    if (!res.ok) {
      throw new Error((data && data.error) || ("HTTP " + res.status));
    }
    return data || {};
  }

  function bindLogout() {
    var btn = document.getElementById("logout-btn");
    if (!btn) return;
    btn.addEventListener("click", async function () {
      try {
        await fetch("/api/auth/logout", { method: "POST" });
      } catch (e) {}
      window.location.assign("/login");
    });
  }

  function initLogin() {
    var form = document.getElementById("login-form");
    var input = document.getElementById("password");
    var btn = document.getElementById("login-btn");
    var errEl = document.getElementById("login-error");
    if (!form || !input) return;

    form.addEventListener("submit", async function (e) {
      e.preventDefault();
      errEl.classList.add("hidden");
      errEl.textContent = "";
      btn.disabled = true;
      try {
        var res = await fetch("/api/auth/login", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ password: input.value }),
        });
        var data = {};
        try { data = await res.json(); } catch (err) {}
        if (!res.ok) {
          errEl.textContent = data.error || "Invalid password";
          errEl.classList.remove("hidden");
          return;
        }
        window.location.assign("/dashboard");
      } catch (err) {
        errEl.textContent = err.message || "Login failed";
        errEl.classList.remove("hidden");
      } finally {
        btn.disabled = false;
      }
    });
  }

  async function initOverview() {
    var healthEl = document.getElementById("stat-health");
    var keysEl = document.getElementById("stat-keys");
    var providersEl = document.getElementById("stat-providers");
    var combosEl = document.getElementById("stat-combos");
    var results = await Promise.allSettled([
      apiFetch("/api/health"),
      apiFetch("/api/keys"),
      apiFetch("/api/providers"),
      apiFetch("/api/combos"),
    ]);
    if (healthEl) {
      healthEl.textContent = results[0].status === "fulfilled" && results[0].value.status === "ok" ? "Online" : "Degraded";
    }
    if (keysEl) {
      var keys = results[1].status === "fulfilled" && Array.isArray(results[1].value.keys) ? results[1].value.keys : [];
      keysEl.textContent = String(keys.length);
    }
    if (providersEl) {
      var conns = results[2].status === "fulfilled" && Array.isArray(results[2].value.connections) ? results[2].value.connections : [];
      providersEl.textContent = String(conns.length);
    }
    if (combosEl) {
      var combos = results[3].status === "fulfilled" && Array.isArray(results[3].value.combos) ? results[3].value.combos : [];
      combosEl.textContent = String(combos.length);
    }
  }

  async function initEndpoint() {
    var baseUrlEl = document.getElementById("base-url");
    var copyBaseBtn = document.getElementById("copy-base-url");
    var tbody = document.getElementById("keys-tbody");
    var createForm = document.getElementById("create-key-form");
    var keyNameInput = document.getElementById("key-name");
    var createdBox = document.getElementById("created-key-box");
    var createdValue = document.getElementById("created-key-value");
    var copyCreatedBtn = document.getElementById("copy-created-key");

    var baseUrl = window.location.origin + "/v1";
    if (baseUrlEl) baseUrlEl.textContent = baseUrl;
    if (copyBaseBtn) {
      copyBaseBtn.addEventListener("click", function () {
        copyText(baseUrl, copyBaseBtn);
      });
    }
    if (copyCreatedBtn && createdValue) {
      copyCreatedBtn.addEventListener("click", function () {
        copyText(createdValue.textContent, copyCreatedBtn);
      });
    }

    async function loadKeys() {
      if (!tbody) return;
      tbody.innerHTML = '<tr><td colspan="6" class="mono">Loading...</td></tr>';
      try {
        var data = await apiFetch("/api/keys");
        var list = Array.isArray(data.keys) ? data.keys : [];
        if (list.length === 0) {
          tbody.innerHTML = '<tr><td colspan="6">No API keys yet. Create one above.</td></tr>';
          return;
        }
        tbody.innerHTML = "";
        list.forEach(function (k, idx) {
          var tr = document.createElement("tr");
          try {
            if (!k || typeof k !== "object") throw new Error("Invalid key record");
            var id = String(k.id || "");
            var name = k.name ? String(k.name) : "(unnamed)";
            var rawKey = String(k.key || "");
            var masked = rawKey.length > 10 ? rawKey.slice(0, 6) + "..." + rawKey.slice(-4) : rawKey;
            var active = k.isActive !== false;
            var allow = (k.access && Array.isArray(k.access.allow)) ? k.access.allow : [];
            var restricted = !!(k.access && k.access.restricted);
            var accessLabel = restricted ? ("Restricted (" + allow.length + ")") : "All models";
            var quota = k.quota ? "Quota set" : "-";
            var limits = k.limits ? "Limits set" : "-";

            tr.innerHTML =
              "<td>" + escapeHtml(name) + "</td>" +
              '<td class="mono">' + escapeHtml(masked) + ' <button type="button" class="btn btn-sm copy-key-btn" aria-label="Copy API key for ' + escapeHtml(name) + '">Copy</button></td>' +
              '<td><span class="badge ' + (active ? "badge-ok" : "badge-off") + '">' + (active ? "Active" : "Paused") + "</span></td>" +
              "<td>" + escapeHtml(accessLabel) + "</td>" +
              "<td>" + escapeHtml(quota + " / " + limits) + "</td>" +
              '<td class="row">' +
                '<button type="button" class="btn btn-sm toggle-key-btn">' + (active ? "Pause" : "Enable") + "</button>" +
                '<button type="button" class="btn btn-sm btn-danger delete-key-btn">Delete</button>' +
              "</td>";

            var copyBtn = tr.querySelector(".copy-key-btn");
            if (copyBtn) {
              copyBtn.addEventListener("click", function () {
                copyText(rawKey, copyBtn);
              });
            }
            var toggleBtn = tr.querySelector(".toggle-key-btn");
            if (toggleBtn) {
              toggleBtn.addEventListener("click", async function () {
                toggleBtn.disabled = true;
                try {
                  await apiFetch("/api/keys/" + encodeURIComponent(id), {
                    method: "PUT",
                    headers: { "Content-Type": "application/json" },
                    body: JSON.stringify({ isActive: !active }),
                  });
                  await loadKeys();
                } catch (err) {
                  showBanner("Failed to update key: " + err.message, "error");
                  toggleBtn.disabled = false;
                }
              });
            }
            var delBtn = tr.querySelector(".delete-key-btn");
            if (delBtn) {
              delBtn.addEventListener("click", async function () {
                delBtn.disabled = true;
                try {
                  await apiFetch("/api/keys/" + encodeURIComponent(id), { method: "DELETE" });
                  await loadKeys();
                } catch (err) {
                  showBanner("Failed to delete key: " + err.message, "error");
                  delBtn.disabled = false;
                }
              });
            }
          } catch (rowErr) {
            tr.innerHTML =
              '<td colspan="6" class="badge-warn">Error rendering key #' + (idx + 1) + ": " + escapeHtml(rowErr.message || "Malformed data") + "</td>";
          }
          tbody.appendChild(tr);
        });
      } catch (err) {
        showBanner("Failed to load keys: " + err.message, "error");
        tbody.innerHTML = '<tr><td colspan="6">Unable to load keys.</td></tr>';
      }
    }

    if (createForm && keyNameInput) {
      createForm.addEventListener("submit", async function (e) {
        e.preventDefault();
        clearBanner();
        var name = keyNameInput.value.trim();
        if (!name) {
          showBanner("Key name is required.", "error");
          keyNameInput.focus();
          return;
        }
        try {
          var created = await apiFetch("/api/keys", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ name: name }),
          });
          keyNameInput.value = "";
          if (createdBox && createdValue && created.key) {
            createdValue.textContent = created.key;
            createdBox.classList.remove("hidden");
          }
          await loadKeys();
        } catch (err) {
          showBanner("Failed to create key: " + err.message, "error");
        }
      });
    }

    await loadKeys();
  }

  async function initProviders() {
    var tbody = document.getElementById("providers-tbody");
    if (!tbody) return;
    try {
      var data = await apiFetch("/api/providers");
      var list = Array.isArray(data.connections) ? data.connections : [];
      if (list.length === 0) {
        tbody.innerHTML = '<tr><td colspan="4">No provider connections configured.</td></tr>';
        return;
      }
      tbody.innerHTML = "";
      list.forEach(function (c) {
        var tr = document.createElement("tr");
        var active = c && c.isActive !== false;
        tr.innerHTML =
          '<td class="mono">' + escapeHtml((c && c.provider) || "-") + "</td>" +
          "<td>" + escapeHtml((c && c.name) || "(unnamed)") + "</td>" +
          '<td class="mono">' + escapeHtml((c && c.id) || "-") + "</td>" +
          '<td><span class="badge ' + (active ? "badge-ok" : "badge-off") + '">' + (active ? "Active" : "Inactive") + "</span></td>";
        tbody.appendChild(tr);
      });
    } catch (err) {
      showBanner("Failed to load providers: " + err.message, "error");
      tbody.innerHTML = '<tr><td colspan="4">Unable to load providers.</td></tr>';
    }
  }

  async function initCombos() {
    var tbody = document.getElementById("combos-tbody");
    if (!tbody) return;
    try {
      var data = await apiFetch("/api/combos");
      var list = Array.isArray(data.combos) ? data.combos : [];
      if (list.length === 0) {
        tbody.innerHTML = '<tr><td colspan="4">No combos configured.</td></tr>';
        return;
      }
      tbody.innerHTML = "";
      list.forEach(function (c) {
        var tr = document.createElement("tr");
        var models = c && Array.isArray(c.models) ? c.models.join(", ") : "-";
        tr.innerHTML =
          "<td>" + escapeHtml((c && c.name) || "-") + "</td>" +
          "<td>" + escapeHtml((c && c.kind) || "default") + "</td>" +
          '<td class="mono">' + escapeHtml(models) + "</td>" +
          '<td class="mono">' + escapeHtml((c && c.updatedAt) || "-") + "</td>";
        tbody.appendChild(tr);
      });
    } catch (err) {
      showBanner("Failed to load combos: " + err.message, "error");
      tbody.innerHTML = '<tr><td colspan="4">Unable to load combos.</td></tr>';
    }
  }

  document.addEventListener("DOMContentLoaded", function () {
    bindLogout();
    var page = document.body.getAttribute("data-page");
    if (page === "login") initLogin();
    else if (page === "overview") initOverview();
    else if (page === "endpoint") initEndpoint();
    else if (page === "providers") initProviders();
    else if (page === "combos") initCombos();
  });
})();
