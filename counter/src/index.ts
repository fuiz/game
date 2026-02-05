import { DurableObject, WorkerEntrypoint } from 'cloudflare:workers';

type CountersObject = {
	[key: string]: number;
};

export class Counter extends DurableObject {
	async increment(name: string): Promise<number> {
		const counters = await this.getAll();
		counters[name] = (counters[name] || 0) + 1;
		// no need to await here, read: https://blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three/
		this.ctx.storage.put('counters', counters);
		return counters[name];
	}

	async get(name: string): Promise<number> {
		return (await this.getAll())[name] || 0;
	}

	async getAll(): Promise<CountersObject> {
		return (await this.ctx.storage.get('counters')) || {};
	}
}

export default class extends WorkerEntrypoint<Env> {
	async fetch(request: Request): Promise<Response> {
		let name = request.url.split('/').pop();

		if (!name) {
			return new Response('No name provided', { status: 400 });
		}

		if (request.method === 'POST') {
			const stub = this.env.COUNTER.getByName('default');

			await stub.increment(name);

			return new Response('OK', { status: 200 });
		} else if (request.method === 'GET') {
			const cache = caches.default;

			let response = await cache.match(request);

			if (response) return response;

			const stub = this.env.COUNTER.getByName('default');

			let count = await stub.get(name);

			response = new Response(JSON.stringify(count), { status: 200 });

			await cache.put(request, response.clone());

			return response;
		} else {
			return new Response('Method not allowed', { status: 405 });
		}
	}

	async getCount(name: string): Promise<number> {
		return await this.env.COUNTER.getByName('default').get(name);
	}

	async getAllCounts(): Promise<CountersObject> {
		return await this.env.COUNTER.getByName('default').getAll();
	}
}
