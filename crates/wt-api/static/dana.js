// The dana panel without a page load (Spec 06, Answer page structure, item 8). Without this
// script the form posts and the page reloads with the split shown.
(function () {
  "use strict";
  var form = document.querySelector("form[data-dana]");
  var result = document.getElementById("dana-result");
  if (!form || !result || !window.fetch || !window.URLSearchParams) {
    return;
  }
  function body(extra) {
    var data = new URLSearchParams(new FormData(form));
    if (extra) {
      data.append(extra, "1");
    }
    return data;
  }
  // The first use of the panel logs panel_open, once.
  var opened = false;
  form.addEventListener("focusin", function () {
    if (opened) {
      return;
    }
    opened = true;
    var first = form.querySelector("input[name=first]");
    if (first) {
      first.parentNode.removeChild(first);
    }
    fetch(form.action, { method: "POST", body: body("open"), credentials: "same-origin" });
  });
  // The split replaces the polite live region, and focus moves to its heading.
  form.addEventListener("submit", function (event) {
    event.preventDefault();
    fetch(form.action, {
      method: "POST",
      body: body(null),
      credentials: "same-origin",
      headers: { "HX-Request": "true" }
    })
      .then(function (response) {
        if (!response.ok) {
          throw new Error("dana " + response.status);
        }
        return response.text();
      })
      .then(function (html) {
        result.innerHTML = html;
        var heading = document.getElementById("dana-result-h");
        if (heading) {
          heading.focus();
        }
      })
      .catch(function () {
        form.submit();
      });
  });
})();
