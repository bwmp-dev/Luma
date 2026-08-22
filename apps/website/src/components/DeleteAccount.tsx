import { ExternalLink, Trash2 } from 'lucide-react';
import { Link } from 'react-router-dom';
import { ACCOUNT_CONSOLE_URL, SUPPORT_EMAIL } from '../config';
import { Footer } from './Footer';

const deleted = [
  'Your encrypted sync data and every stored revision.',
  'Shared vaults and collaboration rooms you own, including for the people you shared them with.',
  'Collaboration room snapshots and live session state.',
  'Your registered devices, memberships, and encryption keys on both services.',
  'Your Luma Cloud account record itself.',
];

const kept = [
  'Hosts, keys, identities, and snippets stored on your own devices. Luma is a local-first client and this data never required an account.',
  'Vaults owned by other people that were shared with you. You are removed as a member; the vault stays with its owner.',
  'Copies other members already synced to their own devices.',
  'Backups and records we must keep for a limited period for legal or security reasons, as described in the privacy policy.',
];

export function DeleteAccount() {
  return (
    <div className='min-h-screen bg-background'>
      <a
        href='#main'
        className='sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-100 focus:rounded-lg focus:bg-accent focus:px-4 focus:py-2 focus:text-sm focus:font-semibold focus:text-accent-foreground'
      >
        Skip to content
      </a>

      <header className='border-b border-border/60 bg-background/70 backdrop-blur-md'>
        <div className='mx-auto flex h-16 max-w-6xl items-center justify-between px-4 sm:px-6 lg:px-8'>
          <Link to='/' className='flex items-center gap-2.5 rounded-md focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-accent'>
            <img src='/logo.png' alt='' width={32} height={32} className='h-8 w-8 rounded-md' />
            <span className='text-lg font-semibold tracking-tight'>Luma</span>
          </Link>
          <Link to='/' className='text-sm text-muted transition-colors hover:text-foreground'>Back to Luma</Link>
        </div>
      </header>

      <main id='main' className='mx-auto w-full max-w-4xl px-4 py-16 sm:px-6 sm:py-24 lg:px-8'>
        <div className='mb-14 max-w-2xl'>
          <div className='mb-5 inline-flex h-12 w-12 items-center justify-center rounded-xl border border-accent/20 bg-accent/10 text-accent'>
            <Trash2 className='h-6 w-6' aria-hidden='true' />
          </div>
          <h1 className='text-4xl font-semibold tracking-tight sm:text-5xl'>Delete your account</h1>
          <p className='mt-5 text-lg leading-8 text-muted'>
            A Luma account is optional and only powers cloud sync and collaborative terminals. You can delete it, and the data it holds, at any time.
          </p>
        </div>

        <section aria-labelledby='in-app-heading' className='mb-14'>
          <h2 id='in-app-heading' className='text-2xl font-semibold tracking-tight'>From the Luma app</h2>
          <p className='mt-4 leading-7 text-muted'>
            This is the quickest route and works on every platform.
          </p>
          <ol className='mt-5 space-y-3 leading-7 text-muted'>
            <li>
              <span className='font-medium text-foreground'>1.</span> Open Luma and go to Settings → Account. On iPhone, iPad, and Android this is the Profile tab, then Luma Account.
            </li>
            <li>
              <span className='font-medium text-foreground'>2.</span> Choose <span className='font-medium text-foreground'>Delete account</span>, read what will be removed, and type DELETE to confirm.
            </li>
            <li>
              <span className='font-medium text-foreground'>3.</span> Luma erases your cloud data and signs you out, then opens your account page so you can delete the sign-in itself.
            </li>
          </ol>
        </section>

        <section aria-labelledby='identity-heading' className='mb-14'>
          <h2 id='identity-heading' className='text-2xl font-semibold tracking-tight'>Deleting the sign-in itself</h2>
          <p className='mt-4 leading-7 text-muted'>
            Your sign-in is held by our identity service, separately from your Luma data. Deleting it is the last step, and you can do it directly at any time. Deleting the sign-in on its own does not remove your Luma Cloud data, so use the in-app deletion above first.
          </p>
          <a
            href={ACCOUNT_CONSOLE_URL}
            target='_blank'
            rel='noreferrer noopener'
            className='mt-5 inline-flex items-center gap-2 font-medium text-accent hover:text-accent-strong'
          >
            Open your account page
            <ExternalLink className='h-4 w-4' aria-hidden='true' />
          </a>
        </section>

        <section aria-labelledby='scope-heading' className='mb-14 grid gap-5 sm:grid-cols-2'>
          <div className='rounded-2xl border border-border bg-surface p-6'>
            <h2 id='scope-heading' className='text-xl font-semibold'>What is deleted</h2>
            <ul className='mt-4 space-y-3 leading-7 text-muted'>
              {deleted.map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          </div>
          <div className='rounded-2xl border border-border bg-surface p-6'>
            <h2 className='text-xl font-semibold'>What is kept</h2>
            <ul className='mt-4 space-y-3 leading-7 text-muted'>
              {kept.map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          </div>
        </section>

        <section aria-labelledby='no-app-heading' className='mb-14'>
          <h2 id='no-app-heading' className='text-2xl font-semibold tracking-tight'>If you no longer have the app</h2>
          <p className='mt-4 leading-7 text-muted'>
            You do not need to reinstall Luma. Use your account page above, or email us from the address on the account and we will delete it for you. We may need to confirm you control the account before acting on the request.
          </p>
          <a href={`mailto:${SUPPORT_EMAIL}`} className='mt-5 inline-block font-medium text-accent hover:text-accent-strong'>
            {SUPPORT_EMAIL}
          </a>
          <p className='mt-6 leading-7 text-muted'>
            Deletion is immediate and cannot be undone. See the{' '}
            <Link to='/privacy' className='font-medium text-accent hover:text-accent-strong'>privacy policy</Link>{' '}
            for how long backups and legally required records are retained.
          </p>
        </section>
      </main>

      <Footer />
    </div>
  );
}
