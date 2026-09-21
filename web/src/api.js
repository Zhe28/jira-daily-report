const BASE = '/api'

async function request(method, path, body) {
  const opts = { method, headers: {} }
  if (body !== undefined) {
    opts.headers['Content-Type'] = 'application/json'
    opts.body = JSON.stringify(body)
  }
  const resp = await fetch(BASE + path, opts)
  const data = await resp.json()
  if (!resp.ok) {
    throw { status: resp.status, error: data.error || '请求失败' }
  }
  return data
}

export function getHealth() {
  return request('GET', '/health')
}

export function getStatus() {
  return request('GET', '/status')
}

export function getConfig() {
  return request('GET', '/config')
}

export function putConfig(cfg) {
  return request('PUT', '/config', cfg)
}
