const fetchHttps = async (url, request) => {
  const body = JSON.parse(request.body);
  const roles = body.messages.map(message => `${message.role}=${message.content}`).join('|');
  if (url !== 'https://api.minimax.io/anthropic/v1/messages' || request.redirect !== 'error' ||
      body.system !== 'Fixture system' || body.model !== 'Fixture-Model' ||
      roles !== 'user=prior|assistant=reply|user=--base-url bad' ||
      new Headers(request.headers).get('x-api-key') !== 'synthetic-fixture-key') {
    throw new Error('Unexpected MiniMax fixture request');
  }
  return new Response(JSON.stringify({ content: [
    { type: 'thinking', thinking: 'Fixture reasoning' },
    { type: 'text', text: 'Fixture answer' },
  ] }), { headers: { 'Content-Type': 'application/json' } });
};
