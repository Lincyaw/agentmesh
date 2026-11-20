const status = document.querySelector('#copy-status');
for (const button of document.querySelectorAll('[data-copy]')) {
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(button.dataset.copy);
      button.textContent = 'Copied';
      status.textContent = 'Command copied to clipboard.';
      setTimeout(() => { button.textContent = 'Copy'; }, 1600);
    } catch {
      status.textContent = 'Clipboard unavailable. Select and copy the command above.';
    }
  });
}
for (const button of document.querySelectorAll('[data-cli]')) {
  button.addEventListener('click', () => {
    for (const other of document.querySelectorAll('[data-cli]')) {
      const selected = other === button;
      other.classList.toggle('selected', selected);
      other.setAttribute('aria-pressed', String(selected));
    }
    document.querySelector('#cli-command').textContent = button.dataset.cli;
    document.querySelector('#copy-cli').dataset.copy = button.dataset.cli;
    document.querySelector('#copy-cli').setAttribute('aria-label', `Copy ${button.dataset.cli} command`);
  });
}
const examples = {
  direct: ['YOU → CLAUDE / LAPTOP', 'Find the Codex session on my server. Ask it to review the migration and reply here.', 'CODEX / SERVER', 'I found two migration risks. I’ll send the review in this conversation.', 'list_sessions', 'send_message', 'Messages retain the sender, conversation, and reply reference.'],
  group: ['YOU → PROJECT GROUP', 'Send the release checklist to the sessions in our project-team group.', 'PROJECT-TEAM / MULTIPLE MACHINES', 'The message is queued for each session in the group. Every recipient can reply in this conversation.', 'set_group', 'send_message', 'Group sends snapshot the local address list into durable per-recipient deliveries.'],
};
for (const button of document.querySelectorAll('[data-example]')) {
  button.addEventListener('click', () => {
    for (const other of document.querySelectorAll('[data-example]')) {
      const selected = other === button;
      other.classList.toggle('selected', selected);
      other.setAttribute('aria-pressed', String(selected));
    }
    const [from, request, to, reply, toolA, toolB, note] = examples[button.dataset.example];
    const content = document.querySelector('#chat-content');
    content.querySelector('.request .sender').textContent = from;
    content.querySelector('.request p').textContent = request;
    content.querySelector('.response .sender').textContent = to;
    content.querySelector('.response p').textContent = reply;
    const tools = content.querySelectorAll('.route-label code');
    tools[0].textContent = toolA; tools[1].textContent = toolB;
    content.querySelector('.chat-footnote').textContent = note;
  });
}
const video = document.querySelector('#promo');
const toggle = document.querySelector('#motion-toggle');
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
let userPaused = false;
let userRequestedPlayback = false;
let visible = false;
let automaticPauseEvents = 0;
toggle.hidden = false;
const reflectPlayback = () => { toggle.textContent = video.paused ? 'Play animation' : 'Pause animation'; };
const pauseAutomatically = () => {
  if (!video.paused) {
    automaticPauseEvents += 1;
    video.pause();
  }
};
const syncPlayback = () => {
  if (!visible || document.hidden || userPaused || (reducedMotion.matches && !userRequestedPlayback)) {
    pauseAutomatically();
  } else {
    video.play().catch(() => { video.controls = true; });
  }
};
video.addEventListener('play', () => {
  userPaused = false;
  if (reducedMotion.matches) userRequestedPlayback = true;
  reflectPlayback();
});
video.addEventListener('pause', () => {
  if (automaticPauseEvents > 0) automaticPauseEvents -= 1;
  else userPaused = true;
  reflectPlayback();
});
toggle.addEventListener('click', () => {
  if (video.paused) {
    userPaused = false;
    userRequestedPlayback = true;
    video.play().catch(() => { video.controls = true; });
  } else {
    userPaused = true;
    video.pause();
  }
});
reducedMotion.addEventListener('change', () => {
  userRequestedPlayback = false;
  syncPlayback();
});
document.addEventListener('visibilitychange', syncPlayback);
new IntersectionObserver(entries => {
  visible = entries[0].isIntersecting;
  syncPlayback();
}, { threshold: 0.15 }).observe(video);
reflectPlayback();
