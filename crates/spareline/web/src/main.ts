const app = document.querySelector("#app");
if (app === null) {
    throw new Error("index.html has no #app element");
}
app.textContent = "It works.";
