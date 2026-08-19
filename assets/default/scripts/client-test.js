/**
 * @summary Affiche les informations techniques du client dans l'élément `.client-test`.
 *
 * @strategy
 *   – Séparation stricte données statiques / dynamiques : Les nœuds DOM statiques
 *     sont pré-compilés en string (AOT). Seul le nœud dynamique (dimensions) est
 *     isolé et mis en cache.
 *   – Zéro-allocation au runtime : L'utilisation de textContent sur un nœud
 *     isolé évite le re-parsing DOM de innerHTML lors du resize.
 *   – Throttle par Dirty Flag : Remplacement de setTimeout par un state booléen
 *     et requestAnimationFrame pour synchroniser avec la VSync sans allouer de timers.
 *
 * @architectural-decision
 *   – L'isolation native ES Module remplace l'IIFE.
 *   – Ajout d'une routine de destruction (dispose) pour garantir l'absence de fuite
 *     sur l'Event Loop (DOM Level 2 Events).
 *   – Listener passif pour ne pas bloquer le thread de compositing du navigateur.
 */

// ---------------------------------------------------------------------------
// 1. Static Data Layout (AOT)
// ---------------------------------------------------------------------------

const agent = navigator.userAgent;
const agentLow = agent.toLowerCase();

const os = (() => {
	if (agent.includes("Win")) return "Windows";
	if (agent.includes("Android")) return "Android";
	if (agent.includes("like Mac")) return "iOS";
	if (agent.includes("Mac")) return "Macintosh";
	if (agent.includes("Linux")) return "Linux";
	return "Unknown OS";
})();

const browser = (() => {
	if (agentLow.includes("edge")) return "MS Edge";
	if (agentLow.includes("edg/")) return "Edge (Chromium)";
	if (agentLow.includes("opr") && window.opr) return "Opera";
	if (agentLow.includes("chrome") && window.chrome) return "Chrome";
	if (agentLow.includes("trident")) return "MS IE";
	if (agentLow.includes("firefox")) return "Firefox";
	if (agentLow.includes("safari")) return "Safari";
	return "Unknown";
})();

const screenInfo = `${screen.width}×${screen.height}px — ${screen.pixelDepth} bits`;

const staticHtmlPayload =
	`<li>Système d'exploitation&nbsp;: ${os}</li>` +
	`<li>Navigateur&nbsp;: ${browser}</li>` +
	`<li>Résolution écran&nbsp;: ${screenInfo}</li>`;

// ---------------------------------------------------------------------------
// 2. System State
// ---------------------------------------------------------------------------

let elRoot = null;
let elDynWindow = null; // Pointeur direct sur le sous-nœud mutable
let isTickScheduled = false; // Dirty flag

// ---------------------------------------------------------------------------
// 3. Logic / Pipeline
// ---------------------------------------------------------------------------

const mutateWindowDimensions = () => {
	// Mutation directe de la string du text node (0 re-parsing DOM)
	elDynWindow.textContent = `Fenêtre de navigation : ${window.innerWidth}×${window.innerHeight}px`;
	isTickScheduled = false;
};

const onResize = () => {
	if (!isTickScheduled) {
		isTickScheduled = true;
		requestAnimationFrame(mutateWindowDimensions);
	}
};

// ---------------------------------------------------------------------------
// 4. Lifecycle Export
// ---------------------------------------------------------------------------

export const init = () => {
	if (elRoot) return; // Sécurité anti-double-boot

	elRoot = document.querySelector(".client-test");
	if (!elRoot) return;

	// Phase 1 : Injection du layout statique et d'un conteneur vide pour le dynamique
	elRoot.innerHTML = `${staticHtmlPayload}<li class="sys-dyn-window"></li>`;

	// Phase 2 : Capture du pointeur mutable
	elDynWindow = elRoot.querySelector(".sys-dyn-window");

	// Phase 3 : Premier rendu
	mutateWindowDimensions();

	// Phase 4 : Enregistrement hardware (Passive = true pour opti scroll/layout)
	window.addEventListener("resize", onResize, { passive: true });
};

export const dispose = () => {
	if (!elRoot) return;

	window.removeEventListener("resize", onResize);

	// Remise à zéro des pointeurs pour le Garbage Collector
	elRoot = null;
	elDynWindow = null;
	isTickScheduled = false;
};
