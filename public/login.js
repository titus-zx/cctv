const form = document.querySelector("form.login");
const pin = document.getElementById("pin");
pin.addEventListener("input", () => {
  pin.value = pin.value.replace(/\D/g, "").slice(0, 6);
  if (pin.value.length === 6) {
    pin.readOnly = true;
    form.requestSubmit();
  }
});
