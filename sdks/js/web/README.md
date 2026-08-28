# @seyd/web

Framework-agnostic custom elements over `@seyd/core`:

```html
<script type="module">import '@seyd/web';</script>
<seyd-video id="v" robot-id="seyd-demo" signal-url="wss://signal.seyd.io/ws"></seyd-video>
<seyd-hud id="hud"></seyd-hud>
<seyd-connect-error id="err"></seyd-connect-error>
<script type="module">
  const v = document.getElementById('v');
  v.addEventListener('seyd-session', (e) => { hud.session = e.detail; err.session = e.detail; });
</script>
```
