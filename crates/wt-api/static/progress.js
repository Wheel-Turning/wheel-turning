// Live progress while a question runs (Spec 06). Without this script an ask form posts as usual
// and the browser waits for the answer. With it, the form posts with Accept: text/event-stream,
// the main content becomes the progress section from the base template, each stage is marked in
// words and announced once in a polite live region, and the closing event opens the answer or
// writes the page a plain post would have shown. Nothing here animates.
(function () {
  "use strict";
  var template = document.getElementById("progress-template");
  if (!template || !template.content || !window.fetch || !window.TextDecoder ||
      !window.URLSearchParams || !window.ReadableStream) {
    return;
  }
  var busy = false;
  Array.prototype.forEach.call(document.querySelectorAll("form[data-progress]"), function (form) {
    form.addEventListener("submit", function (event) {
      if (busy || event.defaultPrevented) {
        return;
      }
      event.preventDefault();
      busy = true;
      var data = new URLSearchParams(new FormData(form));
      var submitter = event.submitter;
      if (submitter && submitter.name) {
        data.append(submitter.name, submitter.value);
      }
      run(form.action, data);
    });
  });

  function run(action, data) {
    var section = template.content.firstElementChild.cloneNode(true);
    var words = section.dataset;
    var live = section.querySelector("[data-live]");
    var heading = section.querySelector("h1");
    var question = section.querySelector("[data-question]");
    var q = data.get("q");
    if (q) {
      question.textContent = q;
      question.hidden = false;
    }
    Array.prototype.forEach.call(document.querySelectorAll("details.menu[open]"), function (d) {
      d.open = false;
    });
    var main = document.getElementById("main");
    while (main.firstChild) {
      main.removeChild(main.firstChild);
    }
    main.appendChild(section);
    window.scrollTo(0, 0);
    heading.focus();

    var current = null;
    var pending = [];
    var finished = false;

    function row(stage) {
      return section.querySelector('[data-stage="' + stage + '"]');
    }
    function mark(li, state) {
      li.querySelector("[data-state]").textContent = words[state];
      if (state === "now") {
        li.setAttribute("aria-current", "step");
      } else {
        li.removeAttribute("aria-current");
      }
    }
    function announce(label) {
      var text = label + ".";
      if (pending.length) {
        text += " " + pending.join(", ") + ".";
        pending = [];
      }
      live.textContent = text;
    }
    function stage(name) {
      var li = row(name);
      if (!li || li === current) {
        return;
      }
      if (current) {
        mark(current, "done");
      }
      current = li;
      mark(li, "now");
      announce(li.querySelector("[data-name]").textContent);
    }
    function count(name, value) {
      var el = section.querySelector('[data-count="' + name + '"]');
      if (!el) {
        return;
      }
      var phrase = value.toLocaleString("en-US") + " " + (value === 1 ? el.dataset.one : el.dataset.many);
      el.textContent = phrase;
      el.hidden = false;
      pending.push(phrase);
    }
    function done(url) {
      finished = true;
      if (current) {
        mark(current, "done");
      }
      Array.prototype.forEach.call(section.querySelectorAll("[data-stage]"), function (li) {
        if (li.querySelector("[data-state]").textContent === words.waiting) {
          mark(li, "skipped");
        }
      });
      announce(words.opening);
      window.location.assign(url);
    }
    function page(html) {
      finished = true;
      document.open();
      document.write(html);
      document.close();
    }
    function failed() {
      if (finished) {
        return;
      }
      finished = true;
      busy = false;
      var box = section.querySelector("[data-failed]");
      // "Try again" posts the same form without the script, which waits for the answer.
      var retry = box.querySelector("form");
      retry.action = action;
      data.forEach(function (value, name) {
        var input = document.createElement("input");
        input.type = "hidden";
        input.name = name;
        input.value = value;
        retry.appendChild(input);
      });
      box.hidden = false;
      live.textContent = box.querySelector("p").textContent;
    }
    function handle(block) {
      var name = "message";
      var lines = [];
      block.split("\n").forEach(function (line) {
        if (line.indexOf("event:") === 0) {
          name = line.slice(6).trim();
        } else if (line.indexOf("data:") === 0) {
          lines.push(line.slice(5).replace(/^ /, ""));
        }
      });
      if (!lines.length) {
        return;
      }
      var body = JSON.parse(lines.join("\n"));
      if (name === "stage") {
        stage(body.stage);
      } else if (name === "count") {
        count(body.name, body.value);
      } else if (name === "done") {
        done(body.url);
      } else if (name === "page") {
        page(body.html);
      }
    }
    function read(reader) {
      var decoder = new TextDecoder();
      var buffer = "";
      function pump() {
        return reader.read().then(function (chunk) {
          if (chunk.done) {
            failed();
            return;
          }
          buffer += decoder.decode(chunk.value, { stream: true }).replace(/\r\n?/g, "\n");
          var at;
          while ((at = buffer.indexOf("\n\n")) !== -1) {
            var block = buffer.slice(0, at);
            buffer = buffer.slice(at + 2);
            handle(block);
            if (finished) {
              return;
            }
          }
          return pump();
        });
      }
      return pump();
    }

    fetch(action, {
      method: "POST",
      body: data,
      credentials: "same-origin",
      redirect: "manual",
      headers: { Accept: "text/event-stream" }
    }).then(function (res) {
      var type = res.headers.get("content-type") || "";
      if (type.indexOf("text/event-stream") === 0) {
        return read(res.body.getReader());
      }
      if (res.type === "opaqueredirect" || !type) {
        failed();
        return;
      }
      return res.text().then(page);
    }).catch(failed);
  }
})();
