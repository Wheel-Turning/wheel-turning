// The answer page's small additions (Spec 06, Answer page structure, rules 2, 5 and 6). The page
// works without this script: the question stays in its heading, Cite opens as a <details> and
// the feedback forms post and reload at the control.
(function () {
  "use strict";

  // Rule 2: once the question's heading scrolls away, a small copy shows under the header. The
  // copy is hidden from assistive technology, so the question is announced once, and it
  // appears without motion. The scroll padding follows the header's height, so a focused
  // control or an anchor target never lands beneath it.
  var sticky = document.querySelector("[data-sticky-q]");
  var heading = document.querySelector("h1.question");
  var header = document.querySelector("header.site");
  function pad() {
    if (header) {
      var extra = sticky && !sticky.hidden ? sticky.offsetHeight : 0;
      document.documentElement.style.scrollPaddingTop = header.offsetHeight + extra + 16 + "px";
    }
  }
  if (sticky && heading && header && window.IntersectionObserver) {
    var observer = new IntersectionObserver(function (entries) {
      var entry = entries[entries.length - 1];
      var away = !entry.isIntersecting && entry.boundingClientRect.top < 0;
      if (sticky.hidden === away) {
        sticky.hidden = !away;
        pad();
      }
    }, { rootMargin: "-" + header.offsetHeight + "px 0px 0px 0px" });
    observer.observe(heading);
    window.addEventListener("resize", pad);
  }
  pad();

  // Rule 4: a summary still being written (owner's request of October 15, 2026). The page came
  // back with the excerpts; this asks for the summary every second for up to 90 seconds and puts
  // it in its place, or removes the place when there is none. Focus is never moved.
  var pending = document.querySelector("[data-summary]");
  var announce = document.querySelector("[data-summary-announce]");
  if (pending && window.fetch) {
    var tries = 0;
    var poll = function () {
      tries += 1;
      fetch(pending.getAttribute("data-summary"), { credentials: "same-origin", cache: "no-store" })
        .then(function (r) {
          if (r.status === 200) {
            return r.text().then(function (html) {
              pending.insertAdjacentHTML("beforebegin", html);
              pending.parentNode.removeChild(pending);
              if (announce) {
                announce.textContent = "The summary is ready, above the excerpts.";
              }
            });
          }
          if (r.status === 202 && tries < 90) {
            window.setTimeout(poll, 1000);
            return null;
          }
          pending.parentNode.removeChild(pending);
          return null;
        })
        .catch(function () {
          if (tries < 90) {
            window.setTimeout(poll, 2000);
          }
        });
    };
    window.setTimeout(poll, 1000);
  }

  // Rule 5: "Copy citation" inside each Cite.
  Array.prototype.forEach.call(document.querySelectorAll("[data-copy]"), function (button) {
    if (!navigator.clipboard) {
      return;
    }
    var source = document.getElementById(button.getAttribute("data-copy"));
    var status = button.parentNode.querySelector(".copy-status");
    if (!source) {
      return;
    }
    button.hidden = false;
    button.addEventListener("click", function () {
      navigator.clipboard.writeText(source.textContent.replace(/\s+/g, " ").trim()).then(
        function () {
          if (status) {
            status.textContent = "Citation copied.";
          }
        },
        function () {
          if (status) {
            status.textContent = "Copying did not work. Select the citation and copy it.";
          }
        }
      );
    });
  });

  // Rule 6: feedback posts without a page load. The status goes to the control's polite live
  // region, the choices clear, and focus stays on the button.
  var EMPTY = "Choose at least one reason or write a note.";
  Array.prototype.forEach.call(document.querySelectorAll("form[data-feedback]"), function (form) {
    var status = form.querySelector(".fb-status");
    if (!status || !window.fetch || !window.URLSearchParams) {
      return;
    }
    function say(text, error) {
      status.textContent = text;
      status.classList.toggle("error", !!error);
    }
    form.addEventListener("submit", function (event) {
      event.preventDefault();
      var note = form.querySelector("textarea[name=note]");
      var chosen = form.querySelector("input[name=reason]:checked");
      if (!chosen && (!note || !note.value.trim())) {
        say(EMPTY, true);
        return;
      }
      fetch(form.action, {
        method: "POST",
        body: new URLSearchParams(new FormData(form)),
        credentials: "same-origin",
        headers: { "HX-Request": "true" }
      })
        .then(function (response) {
          return response.text().then(function (text) {
            if (response.status === 200) {
              form.reset();
              say(text, false);
            } else if (response.status === 422) {
              say(text, true);
            } else {
              throw new Error("feedback " + response.status);
            }
          });
        })
        .catch(function () {
          form.submit();
        });
    });
  });
})();
