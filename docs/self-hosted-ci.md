# CI self-hosted nel pool CPU

## Macchina e stato della migrazione

Il pool condiviso per la CI CPU usa AMD Ryzen 5 3600,
**6 core fisici / 12 thread SMT**, 64 GiB di RAM nominali (circa 62,7 GiB
visibili al sistema) e storage NVMe in RAID1. I thread SMT non sono dodici
core fisici indipendenti.

La configurazione mantiene **quattro slot Linux complessivi**, ripartiti in
**2 KB + 2 altri slot Linux**, e una VM Windows Evaluation separata con **16 GiB di RAM** e
un tetto di **3 CPU**. Altri worker richiedono misure e un nuovo budget globale,
non la moltiplicazione dei limiti. Il riequilibrio iniziale è stato eseguito
senza interrompere i job in corso.

Il 7 ottobre 2026 i due runner CPU KB sono stati registrati sul nuovo host.
Il collaudo runtime ha verificato limiti cgroup, assenza di mount/socket host,
startup persistente e i percorsi Docker reali. L'immagine pubblicata KB `0.48`
ha passato salute, SPA, ricerca, doctor, rifiuto sulla porta non-loopback e
pulizia dello scratch sia keyword-only sia con configurazione predefinita.
Questi smoke non sostituiscono la verifica GitHub della CI sul nuovo commit.

I profili infrastrutturali e i dettagli identificativi della macchina sono
documentati separatamente nella configurazione operativa privata. Il repository
pubblico usa soltanto nomi di pool generici, senza host, percorsi personali o
identificativi di altri progetti. Il gate `PUBLIC_GATE_PATTERNS` resta vincolante;
non modificarlo per ammettere dettagli infrastrutturali nella CI pubblica.

## Instradamento dei workflow KB

Un job che prima usava `ubuntu-latest` seleziona i runner KB con le label
`self-hosted`, `Linux`, `X64`, `kb-cpu`, `cpu-ci` soltanto quando:

1. il repository è esattamente `nicolasacchi/kb`;
2. l'evento non è `pull_request_target`;
3. se l'evento è `pull_request`, il repository di origine della PR coincide con
   il repository corrente.

Quindi push, dispatch, schedule e PR interne ammissibili possono usare il pool CPU.
Le PR da fork, le esecuzioni in altri repository e `pull_request_target` mantengono
il fallback GitHub-hosted `ubuntu-latest`. Gli `if` già presenti sui job restano
vincolanti: scegliere un runner non autorizza un job che prima era escluso.

L'espressione comune per un job Ubuntu x64 è:

```yaml
runs-on: ${{ github.repository == 'nicolasacchi/kb' && github.event_name != 'pull_request_target' && (github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository) && fromJSON('["self-hosted","Linux","X64","kb-cpu","cpu-ci"]') || 'ubuntu-latest' }}
```

Nelle matrici si sostituisce soltanto il runner Ubuntu x64 idoneo; gli altri
valori della matrice non cambiano. **Tutti i job ARM nativi restano GitHub-hosted**,
incluso `ubuntu-24.04-arm`: il pool x64 non può sostituirli. La migrazione non
sposta implicitamente altre piattaforme. I container delle prove first-run
mantengono le distribuzioni originarie: il sistema del runner non è la
distribuzione oggetto del test.

## Confine di fiducia per un repository pubblico

I runner KB sono registrati a livello di repository. Le label e la condizione
YAML sono strumenti di scheduling, **non un confine di sicurezza non aggirabile**:
una PR da fork può modificare il workflow e richiedere direttamente risorse
self-hosted. Il runner repository-scoped non offre una allowlist per workflow.

La policy GitHub di approvazione delle PR da fork è impostata su
`all_external_contributors`: **ogni esecuzione proveniente da un contributore
esterno richiede approvazione manuale**, non soltanto quelle del primo contributo.
Prima di approvare, il maintainer deve controllare il workflow effettivamente
proposto e rifiutare richieste esterne che tentano di usare runner self-hosted,
anche se alterano o rimuovono la condizione di fallback. Non approvare
automaticamente perché una versione precedente della PR era innocua.
Questa procedura non rende il YAML immutabile e non va sostituita da una semplice
verifica delle label.

L'isolamento rootless e i limiti cgroup riducono il rischio operativo ma non
rendono appropriato eseguire codice esterno non fidato sulla macchina condivisa.
Le credenziali di registrazione e i segreti restano fuori dal repository pubblico.

## Budget RAM e CPU condiviso

Il parent cgroup Linux, gestito con slice systemd standard, deve imporre
`MemoryMax=40G` all'insieme dei quattro slot. Il budget è:

| Risorsa | Numero | Tetto RAM combinato per slot | Limiti individuali |
| --- | --- | --- | --- |
| Altri slot Linux | 2 | 9 GiB | client 1 GiB, daemon rootless DinD 8 GiB |
| Linux KB | 2 | 11 GiB | client 10 GiB, daemon rootless DinD 9 GiB |
| VM Windows | 1 | 16 GiB | distinta dal parent Linux |

Per KB, **10 + 9 non significa 19 GiB disponibili**: client e daemon condividono
il parent hard da 11 GiB. I job Rust KB compilano direttamente nel client, mentre
i job degli altri slot Linux usano soprattutto il daemon DinD; assegnare al client KB solo
1 GiB sarebbe quindi errato. I due parent da 9 GiB e i due parent KB da 11 GiB
sommano esattamente 40 GiB. Con la riserva Windows da 16 GiB rimangono circa
6,7 GiB per host e overhead sulla RAM effettivamente visibile. Lo swap dell'host
non è capacità aggiuntiva per i job: i cgroup Linux non devono permettere swap
illimitato; gli slot sono configurati senza swap.

Il parent Linux ha un tetto di **12 CPU logiche**. I quattro slot non hanno un
tetto CPU fisso: pesi uguali distribuiscono la contesa e consentono al worker
attivo di prendere la capacità lasciata libera dagli altri. Le compilazioni
Cargo dirette KB hanno al massimo quattro processi di build per contenere
la memoria; i limiti RAM restano vincolanti. Windows mantiene il proprio tetto
di 3 CPU. Si usano quote e pesi cgroup standard, non controller adattivi.
**Le quote non riservano core fisici** e non promettono 15 CPU simultanee:
quando la VM lavora, tutti condividono i dodici thread disponibili.

Ogni client usa un daemon Docker rootless separato, con TLS e rete dedicata.
Non montare il socket Docker dell'host, directory dell'host o credenziali private
nei job; non esporre le porte dei daemon. Per una variazione di capacità occorre
prima misurare consumo e headroom e poi ricalcolare il budget globale, non
moltiplicare i limiti individuali o aggiungere worker per svuotare la coda.

## Manutenzione e collaudo

Prima di applicare modifiche infrastrutturali, controllare stato e job attivo dei
runner tramite GitHub e confrontare i limiti effettivi delle slice con i profili.
Per un drain, impedire l'accettazione di nuovi job senza terminare quello in
corso; riconvertire o riavviare lo slot soltanto quando è idle. Una coda in attesa
non autorizza a interrompere lavoro o ad aumentare il budget.

Lo spostamento della manutenzione nel pool CPU non cambia le protezioni applicative:

- `ghcr-gc.yml` continua a escludere le PR dai job distruttivi; il dispatch usa
  il dry-run per default, mentre lo schedule esegue la pulizia. Sono eliminabili
  soltanto versioni con tag tutti `main-*`; release, `latest`, tag misti e versioni
  senza tag restano protette. Il controllo anonimo dei canali resta successivo
  alla GC.
- `lockfile-regen.yml` conserva il dispatch e il vincolo della label
  `regen-lockfiles` sulle sole PR interne, i ref di checkout/push, il DCO sign-off
  e il controllo che la potatura Cargo non cambi le versioni mantenute.
- `repo-hygiene.yml` conserva il comportamento del secret `PUBLIC_GATE_PATTERNS`:
  non ne stampa il contenuto, salta con avviso quando manca su PR da fork o
  Dependabot e fallisce su push a main se è vuoto.

Prima del commit, validare i workflow con diagnostica YAML/actionlint e
self-test comportamentali, quindi esercitare realmente runner, DinD e percorsi
consumer modificati. Verificare la CI GitHub sul nuovo SHA prima di dichiarare
conclusa la migrazione: registrazione e tool installati non provano una CI verde.
Non eseguire GC, rigenerazione dei lockfile o pubblicazione come smoke distruttivi.
