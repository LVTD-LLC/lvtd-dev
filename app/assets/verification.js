(() => {
  const button = document.querySelector('[data-resend-seconds]');
  const status = document.querySelector('[data-resend-status]');
  if (!button || !status) return;
  const deadline = Date.now() + Number(button.dataset.resendSeconds) * 1000;
  const tick = () => {
    const remaining = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
    button.disabled = remaining > 0;
    status.textContent = remaining ? `You can resend in ${remaining} seconds.` : 'You can request a new code now.';
    if (remaining) setTimeout(tick, 1000);
  };
  tick();
})();
