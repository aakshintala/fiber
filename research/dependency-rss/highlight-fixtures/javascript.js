// Fetch the model list with a timeout, then render it into <select>.
const MODELS_URL = "https://models.example.com/v1/models";
const TIMEOUT_MS = 8000;

export class ModelPicker {
  constructor(select) {
    this.select = select;
    this.models = [];
  }

  async load() {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), TIMEOUT_MS);
    try {
      const res = await fetch(MODELS_URL, { signal: ctrl.signal });
      if (!res.ok) {
        throw new Error(`models: HTTP ${res.status}`);
      }
      const body = await res.json();
      this.models = body.data ?? [];
      this.render();
    } catch (err) {
      this.select.innerHTML = `<option>failed: ${err.message}</option>`;
    } finally {
      clearTimeout(timer);
    }
  }

  render() {
    this.select.innerHTML = this.models
      .map((m) => `<option value="${m.id}">${m.name}</option>`)
      .join("\n");
  }
}

export function cheapest(models) {
  return models.slice().sort((a, b) => a.price - b.price)[0];
}
