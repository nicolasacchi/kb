# frozen_string_literal: true

namespace :trade do
  resources :rounds, only: %i[index show] do
    collection do
      get :search_pharmacies
    end
    member do
      post :merge_catalogs
    end
    resources :catalogs, only: [:create]
  end
end
